use super::cleanup::{ensure_branch_released, ensure_task_root_released, ensure_worktree_released};
use super::provision::TaskWorkspaceProvisioner;
use super::provision_tests::{git_stdout, persist_phase, run_git, ProjectFixture};
use super::*;

#[test]
fn cleanup_of_an_unprovisioned_task_is_safe_and_idempotent() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("planned-task");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);

    let cleaned = provisioner.cleanup("planned-task").unwrap();

    assert_eq!(cleaned.phase, TaskWorkspacePhase::Cleaned);
    assert!(!cleaned.root.exists());
    assert!(cleaned.journal.is_empty());
    assert_eq!(provisioner.cleanup("planned-task").unwrap(), cleaned);
}

#[test]
fn cleanup_refuses_a_preexisting_unowned_root_for_an_unprovisioned_task() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("planned-unowned");
    let task = fixture.states.load("planned-unowned").unwrap();
    std::fs::create_dir_all(&task.root).unwrap();
    std::fs::write(task.root.join("keep.txt"), b"not GoWild data\n").unwrap();
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);

    let error = provisioner.cleanup("planned-unowned").unwrap_err();

    assert_eq!(error.code, "task_workspace_cleanup_conflict");
    assert_eq!(
        std::fs::read(task.root.join("keep.txt")).unwrap(),
        b"not GoWild data\n"
    );
    assert_eq!(
        fixture.states.load("planned-unowned").unwrap().phase,
        TaskWorkspacePhase::Planned
    );
}

#[test]
fn cleanup_releases_branches_worktrees_and_root_in_dependency_safe_order() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("clean-task");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let provisioned = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "clean-task",
        )
        .unwrap();
    let activated = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "clean-task",
            "api",
        )
        .unwrap();
    let root = activated.root.clone();

    let cleaned = provisioner.cleanup("clean-task").unwrap();

    assert_eq!(cleaned.phase, TaskWorkspacePhase::Cleaned);
    assert!(!root.exists());
    for (repository_id, repository) in &provisioned.repositories {
        assert!(crate::worktree::local_branch_exists(
            &repository.source_path,
            &cleaned.branch_name(repository_id)
        )
        .is_ok_and(|exists| !exists));
        assert!(!crate::worktree::worktree_list_contains_path(
            &repository.source_path,
            &root.join("repositories").join(repository_id)
        )
        .unwrap());
    }
    let released_worktrees = cleaned
        .journal
        .iter()
        .filter_map(|transition| match &transition.resource {
            OwnedResource::RepositoryWorktree { repository_id, .. }
                if transition.operation == TaskTransitionOperation::Release
                    && transition.state == TaskTransitionState::Applied =>
            {
                Some(repository_id.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(released_worktrees, ["web", "api", "shared"]);
    assert_eq!(provisioner.cleanup("clean-task").unwrap(), cleaned);
}

#[test]
fn cleanup_refuses_dirty_changes_before_recording_any_release() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("dirty-task");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "dirty-task",
        )
        .unwrap();
    let task = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "dirty-task",
            "api",
        )
        .unwrap();
    let checkout = &task.repositories["api"]
        .worktree
        .as_ref()
        .unwrap()
        .checkout_path;
    std::fs::write(checkout.join("keep.txt"), b"dirty and important\n").unwrap();

    let error = provisioner.cleanup("dirty-task").unwrap_err();

    assert_eq!(error.code, "task_workspace_cleanup_dirty");
    assert_eq!(
        std::fs::read(checkout.join("keep.txt")).unwrap(),
        b"dirty and important\n"
    );
    let persisted = fixture.states.load("dirty-task").unwrap();
    assert_eq!(persisted.phase, TaskWorkspacePhase::Ready);
    assert!(persisted
        .journal
        .iter()
        .all(|transition| transition.operation != TaskTransitionOperation::Release));
}

#[test]
fn cleanup_refuses_unpushed_commits_but_accepts_a_tracked_remote_copy() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("push-task");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "push-task",
        )
        .unwrap();
    let task = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "push-task",
            "api",
        )
        .unwrap();
    let repository = &task.repositories["api"];
    let checkout = &repository.worktree.as_ref().unwrap().checkout_path;
    let branch = task.branch_name("api");
    std::fs::write(checkout.join("pushed.txt"), b"reviewable\n").unwrap();
    run_git(checkout, &["add", "pushed.txt"]);
    run_git(checkout, &["commit", "--quiet", "-m", "reviewable change"]);

    assert_eq!(
        provisioner.cleanup("push-task").unwrap_err().code,
        "task_workspace_cleanup_unpushed"
    );

    let remote = fixture.root.join("cleanup-remote.git");
    std::fs::create_dir(&remote).unwrap();
    run_git(&remote, &["init", "--bare", "--quiet"]);
    run_git(
        &repository.source_path,
        &["remote", "add", "cleanup-origin", remote.to_str().unwrap()],
    );
    run_git(
        checkout,
        &["push", "--quiet", "-u", "cleanup-origin", &branch],
    );

    let cleaned = provisioner.cleanup("push-task").unwrap();
    assert_eq!(cleaned.phase, TaskWorkspacePhase::Cleaned);
    assert_eq!(
        git_stdout(&remote, &["rev-parse", &format!("refs/heads/{branch}")]).len(),
        40
    );
}

#[test]
fn cleanup_preserves_unowned_data_in_the_task_root() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("unowned-root");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "unowned-root",
        )
        .unwrap();
    std::fs::write(task.root.join("keep.txt"), b"not owned by cleanup\n").unwrap();

    let error = provisioner.cleanup("unowned-root").unwrap_err();

    assert_eq!(error.code, "task_workspace_cleanup_conflict");
    assert_eq!(
        std::fs::read(task.root.join("keep.txt")).unwrap(),
        b"not owned by cleanup\n"
    );
    assert_eq!(
        fixture.states.load("unowned-root").unwrap().phase,
        TaskWorkspacePhase::Ready
    );
}

#[test]
fn cleanup_reconciles_a_worktree_removed_after_its_durable_release_plan() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("cleanup-crash");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let mut task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "cleanup-crash",
        )
        .unwrap();
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Cleaning);
    let repository = &task.repositories["web"];
    let resource = OwnedResource::RepositoryWorktree {
        repository_id: "web".into(),
        source_path: repository.source_path.clone(),
        checkout_path: task.repository_checkout_path("web"),
        base_commit: repository.base_commit.clone(),
    };
    let expected_revision = task.revision;
    task.plan_transition(TaskTransitionOperation::Release, resource)
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    ensure_worktree_released(&task, "web").unwrap();
    assert!(task.repositories["web"].worktree.is_some());

    let recovered = provisioner.cleanup("cleanup-crash").unwrap();

    assert_eq!(recovered.phase, TaskWorkspacePhase::Cleaned);
    assert!(recovered
        .repositories
        .values()
        .all(|repository| repository.worktree.is_none()));
    assert!(!recovered.root.exists());
}

#[test]
fn cleanup_reconciles_a_branch_removed_after_its_durable_release_plan() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("branch-cleanup-crash");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "branch-cleanup-crash",
        )
        .unwrap();
    let mut task = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "branch-cleanup-crash",
            "api",
        )
        .unwrap();
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Cleaning);
    let source_path = task.repositories["api"].source_path.clone();
    let base_commit = task.repositories["api"].base_commit.clone();
    let branch = task.branch_name("api");
    let resource = OwnedResource::RepositoryBranch {
        repository_id: "api".into(),
        checkout_path: task.repository_checkout_path("api"),
        branch: branch.clone(),
        base_commit,
    };
    let expected_revision = task.revision;
    task.plan_transition(TaskTransitionOperation::Release, resource)
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    ensure_branch_released(&task, "api").unwrap();
    assert!(task.repositories["api"]
        .worktree
        .as_ref()
        .unwrap()
        .branch
        .is_some());
    assert!(!crate::worktree::local_branch_exists(&source_path, &branch).unwrap());

    let recovered = provisioner.cleanup("branch-cleanup-crash").unwrap();

    assert_eq!(recovered.phase, TaskWorkspacePhase::Cleaned);
    assert!(!recovered.root.exists());
}

#[test]
fn cleanup_retries_a_branch_release_recorded_failed_after_external_completion() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("branch-cleanup-failed");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "branch-cleanup-failed",
        )
        .unwrap();
    let mut task = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "branch-cleanup-failed",
            "api",
        )
        .unwrap();
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Cleaning);
    let repository = &task.repositories["api"];
    let resource = OwnedResource::RepositoryBranch {
        repository_id: "api".into(),
        checkout_path: task.repository_checkout_path("api"),
        branch: task.branch_name("api"),
        base_commit: repository.base_commit.clone(),
    };
    let expected_revision = task.revision;
    let sequence = task
        .plan_transition(TaskTransitionOperation::Release, resource)
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    ensure_branch_released(&task, "api").unwrap();
    provisioner
        .record_failed_transition(&mut task, sequence, "simulated_cleanup_failure")
        .unwrap();

    let recovered = provisioner.cleanup("branch-cleanup-failed").unwrap();

    assert_eq!(recovered.phase, TaskWorkspacePhase::Cleaned);
    assert!(!recovered.root.exists());
}

#[test]
fn cleanup_retries_a_worktree_release_recorded_failed_after_external_completion() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("worktree-cleanup-failed");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let mut task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "worktree-cleanup-failed",
        )
        .unwrap();
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Cleaning);
    let repository = &task.repositories["web"];
    let resource = OwnedResource::RepositoryWorktree {
        repository_id: "web".into(),
        source_path: repository.source_path.clone(),
        checkout_path: task.repository_checkout_path("web"),
        base_commit: repository.base_commit.clone(),
    };
    let expected_revision = task.revision;
    let sequence = task
        .plan_transition(TaskTransitionOperation::Release, resource)
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    ensure_worktree_released(&task, "web").unwrap();
    provisioner
        .record_failed_transition(&mut task, sequence, "simulated_cleanup_failure")
        .unwrap();

    let recovered = provisioner.cleanup("worktree-cleanup-failed").unwrap();

    assert_eq!(recovered.phase, TaskWorkspacePhase::Cleaned);
    assert!(!recovered.root.exists());
}

#[test]
fn cleanup_retries_a_root_release_recorded_failed_after_external_completion() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("root-cleanup-failed");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let mut task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "root-cleanup-failed",
        )
        .unwrap();
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Cleaning);
    for repository_id in ["web", "api", "shared"] {
        let repository = &task.repositories[repository_id];
        let resource = OwnedResource::RepositoryWorktree {
            repository_id: repository_id.into(),
            source_path: repository.source_path.clone(),
            checkout_path: task.repository_checkout_path(repository_id),
            base_commit: repository.base_commit.clone(),
        };
        let expected_revision = task.revision;
        let sequence = task
            .plan_transition(TaskTransitionOperation::Release, resource)
            .unwrap();
        fixture.states.save(&task, expected_revision).unwrap();
        ensure_worktree_released(&task, repository_id).unwrap();
        let expected_revision = task.revision;
        task.finish_transition(sequence, TaskTransitionState::Applied, None)
            .unwrap();
        fixture.states.save(&task, expected_revision).unwrap();
    }
    let resource = OwnedResource::WorkspaceDirectory {
        path: task.root.clone(),
    };
    let expected_revision = task.revision;
    let sequence = task
        .plan_transition(TaskTransitionOperation::Release, resource)
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    ensure_task_root_released(&task).unwrap();
    provisioner
        .record_failed_transition(&mut task, sequence, "simulated_cleanup_failure")
        .unwrap();

    let recovered = provisioner.cleanup("root-cleanup-failed").unwrap();

    assert_eq!(recovered.phase, TaskWorkspacePhase::Cleaned);
    assert!(!recovered.root.exists());
}
