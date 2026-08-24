use std::sync::Arc;

use super::branch::ensure_task_branch;
use super::provision::TaskWorkspaceProvisioner;
use super::provision_tests::{git_stdout, persist_plan, run_git, ProjectFixture};
use super::*;

#[test]
fn activation_branches_only_the_selected_repository_and_allows_normal_commits() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("selective-task");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let provisioned = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "selective-task",
        )
        .unwrap();

    let activated = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "selective-task",
            "api",
        )
        .unwrap();

    assert_eq!(
        activated.repositories["api"]
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.branch.as_deref()),
        Some("gowild/selective-task/api")
    );
    for repository_id in ["shared", "web"] {
        assert_eq!(
            activated.repositories[repository_id]
                .worktree
                .as_ref()
                .and_then(|worktree| worktree.branch.as_ref()),
            None
        );
        assert_eq!(
            git_stdout(
                &activated.repositories[repository_id]
                    .worktree
                    .as_ref()
                    .unwrap()
                    .checkout_path,
                &["branch", "--show-current"],
            ),
            ""
        );
    }
    let api_checkout = &activated.repositories["api"]
        .worktree
        .as_ref()
        .unwrap()
        .checkout_path;
    assert_eq!(
        git_stdout(api_checkout, &["branch", "--show-current"]),
        "gowild/selective-task/api"
    );

    std::fs::write(api_checkout.join("feature.txt"), b"task change\n").unwrap();
    run_git(api_checkout, &["add", "feature.txt"]);
    run_git(api_checkout, &["commit", "--quiet", "-m", "task change"]);
    let resumed = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "selective-task",
            "api",
        )
        .unwrap();
    assert_eq!(resumed, activated);
    assert_ne!(
        git_stdout(api_checkout, &["rev-parse", "HEAD"]),
        provisioned.repositories["api"].base_commit
    );
}

#[test]
fn activation_reconciles_a_branch_created_after_its_durable_plan() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("branch-crash");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let mut task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "branch-crash",
        )
        .unwrap();
    let resource = OwnedResource::RepositoryBranch {
        repository_id: "api".into(),
        checkout_path: task.repository_checkout_path("api"),
        branch: task.branch_name("api"),
        base_commit: task.repositories["api"].base_commit.clone(),
    };
    let sequence = persist_plan(&fixture.states, &mut task, resource);
    ensure_task_branch(&task, "api", false).unwrap();
    assert_eq!(task.journal.last().unwrap().sequence, sequence);
    assert_eq!(
        task.journal.last().unwrap().state,
        TaskTransitionState::Planned
    );
    assert_eq!(
        task.repositories["api"].worktree.as_ref().unwrap().branch,
        None
    );

    let resumed = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "branch-crash",
            "api",
        )
        .unwrap();
    assert_eq!(
        resumed.repositories["api"]
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.branch.as_deref()),
        Some("gowild/branch-crash/api")
    );
    assert_eq!(
        resumed.journal.last().unwrap().state,
        TaskTransitionState::Applied
    );
}

#[test]
fn activation_preserves_an_unowned_branch_and_recovers_after_it_is_removed() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("conflicting-branch");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let provisioned = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "conflicting-branch",
        )
        .unwrap();
    let source = &provisioned.repositories["api"].source_path;
    let base = provisioned.repositories["api"].base_commit.as_str();
    let branch = provisioned.branch_name("api");
    std::fs::write(source.join("conflict.txt"), b"unowned\n").unwrap();
    run_git(source, &["add", "conflict.txt"]);
    run_git(source, &["commit", "--quiet", "-m", "unowned branch"]);
    let conflicting_commit = git_stdout(source, &["rev-parse", "HEAD"]);
    run_git(source, &["reset", "--hard", "--quiet", base]);
    run_git(source, &["branch", &branch, &conflicting_commit]);

    let error = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "conflicting-branch",
            "api",
        )
        .unwrap_err();
    assert_eq!(error.code, "task_repository_branch_conflict");
    assert_eq!(
        git_stdout(source, &["rev-parse", &format!("refs/heads/{branch}")]),
        conflicting_commit
    );
    let persisted = fixture.states.load("conflicting-branch").unwrap();
    assert_eq!(persisted.phase, TaskWorkspacePhase::NeedsAttention);
    assert_eq!(
        persisted.journal.last().unwrap().failure_code.as_deref(),
        Some("task_repository_branch_conflict")
    );

    run_git(source, &["branch", "-D", &branch]);
    let recovered = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "conflicting-branch",
            "api",
        )
        .unwrap();
    assert_eq!(recovered.phase, TaskWorkspacePhase::Ready);
    assert_eq!(
        recovered.repositories["api"]
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.branch.as_deref()),
        Some(branch.as_str())
    );
}

#[test]
fn parallel_tasks_activate_distinct_branches_in_the_same_repository() {
    let fixture = ProjectFixture::new(false);
    for task_id in ["branch-one", "branch-two"] {
        fixture.create_task(task_id);
        TaskWorkspaceProvisioner::new(&fixture.states)
            .provision(
                &fixture.definition,
                &fixture.private_state,
                &fixture.project,
                task_id,
            )
            .unwrap();
    }
    let states = Arc::new(fixture.states.clone());
    let handles = ["branch-one", "branch-two"].map(|task_id| {
        let states = Arc::clone(&states);
        let definition = fixture.definition.clone();
        let private_state = fixture.private_state.clone();
        let project = fixture.project.clone();
        std::thread::spawn(move || {
            TaskWorkspaceProvisioner::new(&states).activate_repository(
                &definition,
                &private_state,
                &project,
                task_id,
                "api",
            )
        })
    });
    let [first, second] = handles;
    let first = first.join().unwrap().unwrap();
    let second = second.join().unwrap().unwrap();

    assert_eq!(
        first.repositories["api"]
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.branch.as_deref()),
        Some("gowild/branch-one/api")
    );
    assert_eq!(
        second.repositories["api"]
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.branch.as_deref()),
        Some("gowild/branch-two/api")
    );
}
