use super::*;
use crate::project::manifest::{LoadedProject, ResolvedRepo};
use crate::project::model::{ProjectManifest, ProjectRepo, PROJECT_MANIFEST_VERSION};
use crate::project::task_workspace::{
    OwnedResource, TaskAgent, TaskProtocol, TaskRoute, TaskTransitionOperation, TaskTransitionState,
};

fn project_fixture() -> LoadedProject {
    let root = std::env::temp_dir().join("gowild-change-set-fixture");
    let repositories = [
        ("web", vec!["api"]),
        ("shared", vec![]),
        ("api", vec!["shared"]),
    ];
    LoadedProject {
        manifest_path: root.join("gowild-project.toml"),
        root: root.clone(),
        digest: "a".repeat(64),
        manifest: ProjectManifest {
            version: PROJECT_MANIFEST_VERSION,
            id: "platform".into(),
            name: "Platform".into(),
            repositories: repositories
                .iter()
                .map(|(id, depends_on)| ProjectRepo {
                    id: (*id).into(),
                    path: PathBuf::from(id),
                    base: None,
                    depends_on: depends_on.iter().map(|value| (*value).into()).collect(),
                })
                .collect(),
            setup: Vec::new(),
            tests: Vec::new(),
            services: Vec::new(),
        },
        repositories: repositories
            .iter()
            .map(|(id, depends_on)| ResolvedRepo {
                id: (*id).into(),
                path: root.join(id),
                configured_base: None,
                base_commit: "1".repeat(40),
                head_commit: "1".repeat(40),
                depends_on: depends_on.iter().map(|value| (*value).into()).collect(),
            })
            .collect(),
    }
}

fn unprovisioned_fixture() -> TaskWorkspace {
    let project = project_fixture();
    let task_store = project.root.join("task-store");
    TaskWorkspace::new(
        &project,
        "task-1",
        "Ship the coordinated feature",
        TaskAgent::Codex,
        TaskRoute {
            gateway_id: "mindshub".into(),
            protocol: TaskProtocol::OpenAiResponses,
            model: "model-a".into(),
        },
        task_store,
    )
    .unwrap()
}

pub(super) fn fixture() -> TaskWorkspace {
    let mut task = unprovisioned_fixture();
    task.transition_phase(TaskWorkspacePhase::Provisioning)
        .unwrap();
    for repository_id in ["shared", "api", "web"] {
        let repository = task.repositories[repository_id].clone();
        let checkout_path = task.repository_checkout_path(repository_id);
        let branch = task.branch_name(repository_id);
        apply(
            &mut task,
            OwnedResource::RepositoryWorktree {
                repository_id: repository_id.into(),
                source_path: repository.source_path,
                checkout_path: checkout_path.clone(),
                base_commit: repository.base_commit.clone(),
            },
        );
        apply(
            &mut task,
            OwnedResource::RepositoryBranch {
                repository_id: repository_id.into(),
                checkout_path,
                branch,
                base_commit: repository.base_commit,
            },
        );
    }
    task.transition_phase(TaskWorkspacePhase::Ready).unwrap();
    task
}

fn apply(task: &mut TaskWorkspace, resource: OwnedResource) {
    let sequence = task
        .plan_transition(TaskTransitionOperation::Acquire, resource)
        .unwrap();
    task.finish_transition(sequence, TaskTransitionState::Applied, None)
        .unwrap();
}

#[test]
fn change_set_starts_pending_in_dependency_order_and_requires_approval() {
    let task = fixture();
    let change_set = ChangeSet::for_task(&task).unwrap();

    assert_eq!(change_set.schema_version, CHANGE_SET_VERSION);
    assert_eq!(change_set.dependency_order, ["shared", "api", "web"]);
    assert!(change_set
        .repositories
        .values()
        .all(|change| change.snapshot == RepositorySnapshot::Pending));
    assert_eq!(change_set.publication.group_id, "platform:task-1");
    assert!(!change_set.merge_is_approved());
    assert!(change_set.affected_repository_ids().is_empty());
}

#[test]
fn affected_repositories_and_merge_order_exclude_clean_repositories() {
    let task = fixture();
    let mut change_set = ChangeSet::for_task(&task).unwrap();
    change_set.repositories.get_mut("shared").unwrap().snapshot = RepositorySnapshot::Unchanged {
        head_commit: "1".repeat(40),
        commits_ahead: 0,
    };
    for repository_id in ["api", "web"] {
        change_set
            .repositories
            .get_mut(repository_id)
            .unwrap()
            .snapshot = RepositorySnapshot::Changed {
            head_commit: "2".repeat(40),
            commits_ahead: 1,
            files: vec![ChangedFile {
                path: PathBuf::from("src/lib.rs"),
                kind: ChangedFileKind::Modified,
                staged: false,
                worktree: true,
            }],
            insertions: 3,
            deletions: 1,
            diff: DiffSummary {
                sha256: "b".repeat(64),
                bytes: 42,
                truncated: false,
            },
        };
    }

    assert_eq!(change_set.affected_repository_ids(), ["api", "web"]);
    assert_eq!(change_set.merge_order(), ["api", "web"]);
}

#[test]
fn change_set_rejects_unprovisioned_and_cleaned_tasks() {
    let task = unprovisioned_fixture();
    let error = ChangeSet::for_task(&task).unwrap_err();
    assert_eq!(error.code, "task_change_set_unavailable");

    let mut task = fixture();
    task.phase = TaskWorkspacePhase::Cleaned;
    let error = ChangeSet::for_task(&task).unwrap_err();
    assert_eq!(error.code, "task_change_set_unavailable");
}

#[test]
fn serialized_change_set_is_versioned_and_rejects_unknown_fields() {
    let change_set = ChangeSet::for_task(&fixture()).unwrap();
    let mut json = serde_json::to_value(&change_set).unwrap();
    assert_eq!(json["schema_version"], CHANGE_SET_VERSION);
    json.as_object_mut()
        .unwrap()
        .insert("future".into(), serde_json::json!(true));
    assert!(serde_json::from_value::<ChangeSet>(json).is_err());
}
