use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::provision::{
    ensure_detached_task_worktree, ensure_owned_task_root, TaskWorkspaceProvisioner,
};
use super::repository::TaskWorkspaceRepository;
use super::tests::route;
use super::*;
use crate::project::manifest::{LoadedProject, ResolvedRepo};
use crate::project::model::{
    ProjectCommand, ProjectManifest, ProjectRepo, PROJECT_MANIFEST_VERSION,
};
use crate::project::{ProjectDefinition, ProjectPrivateState, ProjectPrivateStateRepository};

static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

struct ProjectFixture {
    root: PathBuf,
    definition: ProjectDefinition,
    private_state: ProjectPrivateState,
    project: LoadedProject,
    states: TaskWorkspaceRepository,
}

impl ProjectFixture {
    fn new(executable: bool) -> Self {
        let root = test_root("task-provisioning");
        std::fs::create_dir_all(&root).unwrap();
        let repositories = [
            ("shared", Vec::<String>::new()),
            ("api", vec!["shared".to_string()]),
            ("web", vec!["api".to_string()]),
        ];
        let mut manifest_repositories = Vec::new();
        let mut resolved_repositories = Vec::new();
        for (repository_id, depends_on) in repositories {
            let path = root.join(repository_id);
            let base_commit = create_repository(&path, repository_id);
            manifest_repositories.push(ProjectRepo {
                id: repository_id.to_string(),
                path: PathBuf::from(repository_id),
                base: Some("main".into()),
                depends_on: depends_on.clone(),
            });
            resolved_repositories.push(ResolvedRepo {
                id: repository_id.to_string(),
                path: path.canonicalize().unwrap(),
                configured_base: Some("main".into()),
                base_commit: base_commit.clone(),
                head_commit: base_commit,
                depends_on,
            });
        }
        let setup = executable.then(|| ProjectCommand {
            id: "prepare".into(),
            repository: Some("shared".into()),
            cwd: None,
            argv: vec!["true".into()],
            environment: Default::default(),
        });
        let manifest = ProjectManifest {
            version: PROJECT_MANIFEST_VERSION,
            id: "multi-repo-project".into(),
            name: "Multi-repo project".into(),
            repositories: manifest_repositories,
            setup: setup.into_iter().collect(),
            tests: Vec::new(),
            services: Vec::new(),
        };
        manifest.validate().unwrap();
        let digest = if executable { "b" } else { "a" }.repeat(64);
        let manifest_path = root.join("gowild-project.toml");
        let definition = ProjectDefinition {
            manifest_path: manifest_path.clone(),
            root: root.clone(),
            digest: digest.clone(),
            manifest: manifest.clone(),
        };
        let private_state = ProjectPrivateStateRepository::new(root.join("private-state"))
            .load(&definition)
            .unwrap();
        let project = LoadedProject {
            manifest_path,
            root: root.clone(),
            digest,
            manifest,
            repositories: resolved_repositories,
        };
        let states =
            TaskWorkspaceRepository::new(root.join("task-state"), root.join("task-workspaces"));
        Self {
            root,
            definition,
            private_state,
            project,
            states,
        }
    }

    fn create_task(&self, task_id: &str) -> TaskWorkspace {
        let task = TaskWorkspace::new(
            &self.project,
            task_id,
            "Update the shared contract and both clients",
            TaskAgent::Codex,
            route(),
            self.states.workspace_store_root().to_path_buf(),
        )
        .unwrap();
        self.states.create(&task).unwrap();
        task
    }
}

impl Drop for ProjectFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn provisioner_materializes_every_repo_as_a_detached_checkout() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("task-42");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);

    let provisioned = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "task-42",
        )
        .unwrap();
    assert_eq!(provisioned.phase, TaskWorkspacePhase::Ready);
    let acquired_repositories = provisioned
        .journal
        .iter()
        .filter_map(|transition| match &transition.resource {
            OwnedResource::RepositoryWorktree { repository_id, .. }
                if transition.operation == TaskTransitionOperation::Acquire
                    && transition.state == TaskTransitionState::Applied =>
            {
                Some(repository_id.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(acquired_repositories, ["shared", "api", "web"]);
    for (repository_id, repository) in &provisioned.repositories {
        let worktree = repository.worktree.as_ref().unwrap();
        assert_eq!(
            worktree.checkout_path,
            provisioned.repository_checkout_path(repository_id)
        );
        assert_eq!(worktree.head_commit, repository.base_commit);
        assert_eq!(worktree.branch, None);
        assert!(worktree.checkout_path.join("README.md").exists());
        assert_eq!(
            git_stdout(&worktree.checkout_path, &["branch", "--show-current"]),
            ""
        );
    }

    let resumed = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "task-42",
        )
        .unwrap();
    assert_eq!(resumed, provisioned);
}

#[test]
fn provisioner_reconciles_a_worktree_created_after_its_durable_plan() {
    let fixture = ProjectFixture::new(false);
    let mut task = fixture.create_task("recover-task");
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Provisioning);
    let root_resource = OwnedResource::WorkspaceDirectory {
        path: task.root.clone(),
    };
    let root_sequence = persist_plan(&fixture.states, &mut task, root_resource);
    ensure_owned_task_root(&task).unwrap();
    persist_finish(&fixture.states, &mut task, root_sequence);

    let repository_id = "shared";
    let repository = &task.repositories[repository_id];
    let resource = OwnedResource::RepositoryWorktree {
        repository_id: repository_id.into(),
        source_path: repository.source_path.clone(),
        checkout_path: task.repository_checkout_path(repository_id),
        base_commit: repository.base_commit.clone(),
    };
    let sequence = persist_plan(&fixture.states, &mut task, resource);
    ensure_detached_task_worktree(&task, repository_id).unwrap();
    assert_eq!(task.journal.last().unwrap().sequence, sequence);
    assert_eq!(
        task.journal.last().unwrap().state,
        TaskTransitionState::Planned
    );

    let resumed = TaskWorkspaceProvisioner::new(&fixture.states)
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "recover-task",
        )
        .unwrap();
    assert_eq!(resumed.phase, TaskWorkspacePhase::Ready);
    assert!(resumed
        .journal
        .iter()
        .all(|transition| transition.state != TaskTransitionState::Planned));
    assert!(resumed
        .repositories
        .values()
        .all(|repository| repository.worktree.is_some()));
}

#[test]
fn provisioner_preserves_unowned_checkout_data_and_records_attention() {
    let fixture = ProjectFixture::new(false);
    let task = fixture.create_task("conflict-task");
    ensure_owned_task_root(&task).unwrap();
    let conflict = task.repository_checkout_path("shared");
    std::fs::create_dir(&conflict).unwrap();
    std::fs::write(conflict.join("keep.txt"), b"user data").unwrap();

    let error = TaskWorkspaceProvisioner::new(&fixture.states)
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "conflict-task",
        )
        .unwrap_err();
    assert_eq!(error.code, "task_repository_worktree_conflict");
    assert_eq!(
        std::fs::read(conflict.join("keep.txt")).unwrap(),
        b"user data"
    );
    let persisted = fixture.states.load("conflict-task").unwrap();
    assert_eq!(persisted.phase, TaskWorkspacePhase::NeedsAttention);
    assert_eq!(
        persisted.journal.last().unwrap().failure_code.as_deref(),
        Some("task_repository_worktree_conflict")
    );
}

#[test]
fn provisioner_requires_manifest_trust_before_git_or_filesystem_mutation() {
    let fixture = ProjectFixture::new(true);
    let task = fixture.create_task("untrusted-task");

    let error = TaskWorkspaceProvisioner::new(&fixture.states)
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "untrusted-task",
        )
        .unwrap_err();
    assert_eq!(error.code, "project_manifest_untrusted");
    assert!(!task.root.exists());
    assert_eq!(
        fixture.states.load("untrusted-task").unwrap().phase,
        TaskWorkspacePhase::Planned
    );
}

#[test]
fn provisioner_rejects_a_definition_that_does_not_bind_the_resolved_project() {
    let fixture = ProjectFixture::new(false);
    let task = fixture.create_task("mismatched-definition");
    let mut mismatched = fixture.definition.clone();
    mismatched.digest = "c".repeat(64);

    let error = TaskWorkspaceProvisioner::new(&fixture.states)
        .provision(
            &mismatched,
            &fixture.private_state,
            &fixture.project,
            "mismatched-definition",
        )
        .unwrap_err();
    assert_eq!(error.code, "project_definition_mismatch");
    assert!(!task.root.exists());
}

#[test]
fn provisioner_does_not_adopt_ready_data_without_journal_ownership() {
    let fixture = ProjectFixture::new(false);
    let mut task = fixture.create_task("unjournaled-ready");
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Provisioning);
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Ready);
    ensure_owned_task_root(&task).unwrap();

    let error = TaskWorkspaceProvisioner::new(&fixture.states)
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "unjournaled-ready",
        )
        .unwrap_err();
    assert_eq!(error.code, "task_workspace_ownership_mismatch");
    assert!(task.root.exists());
}

#[test]
fn provisioner_isolates_parallel_tasks_across_the_same_repositories() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("parallel-one");
    fixture.create_task("parallel-two");
    let states = Arc::new(fixture.states.clone());
    let definition = fixture.definition.clone();
    let private_state = fixture.private_state.clone();
    let project = fixture.project.clone();

    let handles = ["parallel-one", "parallel-two"].map(|task_id| {
        let states = Arc::clone(&states);
        let definition = definition.clone();
        let private_state = private_state.clone();
        let project = project.clone();
        std::thread::spawn(move || {
            TaskWorkspaceProvisioner::new(&states).provision(
                &definition,
                &private_state,
                &project,
                task_id,
            )
        })
    });
    let [first_handle, second_handle] = handles;
    let first_task = first_handle.join().unwrap().unwrap();
    let second_task = second_handle.join().unwrap().unwrap();

    assert_ne!(first_task.root, second_task.root);
    assert_ne!(
        first_task.repositories["api"]
            .worktree
            .as_ref()
            .unwrap()
            .checkout_path,
        second_task.repositories["api"]
            .worktree
            .as_ref()
            .unwrap()
            .checkout_path
    );
}

fn persist_phase(
    states: &TaskWorkspaceRepository,
    task: &mut TaskWorkspace,
    phase: TaskWorkspacePhase,
) {
    let expected_revision = task.revision;
    task.transition_phase(phase).unwrap();
    states.save(task, expected_revision).unwrap();
}

fn persist_plan(
    states: &TaskWorkspaceRepository,
    task: &mut TaskWorkspace,
    resource: OwnedResource,
) -> u64 {
    let expected_revision = task.revision;
    let sequence = task
        .plan_transition(TaskTransitionOperation::Acquire, resource)
        .unwrap();
    states.save(task, expected_revision).unwrap();
    sequence
}

fn persist_finish(states: &TaskWorkspaceRepository, task: &mut TaskWorkspace, sequence: u64) {
    let expected_revision = task.revision;
    task.finish_transition(sequence, TaskTransitionState::Applied, None)
        .unwrap();
    states.save(task, expected_revision).unwrap();
}

fn create_repository(path: &Path, name: &str) -> String {
    std::fs::create_dir_all(path).unwrap();
    run_git(path, &["init", "--quiet"]);
    run_git(path, &["config", "user.email", "gowild@example.invalid"]);
    run_git(path, &["config", "user.name", "GoWild Test"]);
    std::fs::write(path.join("README.md"), format!("# {name}\n")).unwrap();
    run_git(path, &["add", "README.md"]);
    run_git(path, &["commit", "--quiet", "-m", "initial"]);
    run_git(path, &["branch", "-M", "main"]);
    git_stdout(path, &["rev-parse", "HEAD"])
}

fn run_git(path: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {} failed", args.join(" "));
}

fn git_stdout(path: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {} failed", args.join(" "));
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn test_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed);
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir());
    temp_root.join(format!(
        "gowild-{label}-{}-{nonce}-{sequence}",
        std::process::id()
    ))
}
