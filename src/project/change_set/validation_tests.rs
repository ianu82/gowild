use std::path::PathBuf;

use super::tests::fixture;
use super::*;

#[test]
fn validation_accepts_current_and_explicitly_marks_older_task_snapshots() {
    let mut task = fixture();
    let change_set = ChangeSet::for_task(&task).unwrap();

    change_set.validate_for_task(&task).unwrap();
    assert!(!change_set.is_stale_for_task(&task));

    task.transition_phase(TaskWorkspacePhase::Running).unwrap();
    change_set.validate_for_task(&task).unwrap();
    assert!(change_set.is_stale_for_task(&task));
}

#[test]
fn validation_accepts_checked_planned_and_published_review_state() {
    let task = fixture();
    let mut change_set = changed_change_set(&task);
    change_set.checks.insert(
        "api-test".into(),
        ChangeSetCheck {
            command_id: "api-test".into(),
            repository_id: Some("api".into()),
            status: CheckStatus::Passed,
            duration_ms: Some(12),
            exit_code: Some(0),
            failure_code: None,
        },
    );
    change_set
        .plan_draft_pull_requests(&std::collections::BTreeMap::from([(
            "api".into(),
            "main".into(),
        )]))
        .unwrap();
    let plan = change_set.publication.planned_pull_requests["api"].clone();
    change_set.publication.draft_pull_requests.insert(
        "api".into(),
        DraftPullRequest {
            repository_id: "api".into(),
            number: 41,
            url: "https://github.com/example/api/pull/41".into(),
            head_branch: plan.head_branch,
            base_branch: plan.base_branch,
            state: PullRequestState::Draft,
        },
    );
    change_set.merge_gate = MergeGate::Approved {
        approval: MergeApproval::Human {
            actor: "reviewer@example.invalid".into(),
        },
    };

    change_set.validate_for_task(&task).unwrap();
}

#[test]
fn validation_rejects_redirected_checkouts_and_hostile_file_paths() {
    let task = fixture();
    let mut redirected = ChangeSet::for_task(&task).unwrap();
    redirected
        .repositories
        .get_mut("api")
        .unwrap()
        .checkout_path = PathBuf::from("/tmp/other");
    assert_invalid(&redirected, &task);

    let mut hostile = changed_change_set(&task);
    let RepositorySnapshot::Changed { files, .. } =
        &mut hostile.repositories.get_mut("api").unwrap().snapshot
    else {
        unreachable!();
    };
    files[0].path = PathBuf::from("../outside");
    assert_invalid(&hostile, &task);
}

#[test]
fn validation_rejects_unbounded_or_inconsistent_diff_and_check_state() {
    let task = fixture();
    let mut change_set = changed_change_set(&task);
    let RepositorySnapshot::Changed { diff, .. } =
        &mut change_set.repositories.get_mut("api").unwrap().snapshot
    else {
        unreachable!();
    };
    diff.bytes = 2 * 1024 * 1024;
    diff.truncated = false;
    assert_invalid(&change_set, &task);

    let mut invalid_check = changed_change_set(&task);
    invalid_check.checks.insert(
        "api-test".into(),
        ChangeSetCheck {
            command_id: "different".into(),
            repository_id: Some("api".into()),
            status: CheckStatus::Passed,
            duration_ms: Some(1),
            exit_code: Some(0),
            failure_code: None,
        },
    );
    assert_invalid(&invalid_check, &task);
}

#[test]
fn validation_rejects_hostile_review_and_approval_state() {
    let task = fixture();
    let mut change_set = changed_change_set(&task);
    change_set
        .plan_draft_pull_requests(&std::collections::BTreeMap::from([(
            "api".into(),
            "main".into(),
        )]))
        .unwrap();
    change_set
        .publication
        .planned_pull_requests
        .get_mut("api")
        .unwrap()
        .base_branch = "--upload-pack=evil".into();
    assert_invalid(&change_set, &task);

    let mut hostile_url = changed_change_set(&task);
    hostile_url
        .plan_draft_pull_requests(&std::collections::BTreeMap::from([(
            "api".into(),
            "main".into(),
        )]))
        .unwrap();
    let plan = hostile_url.publication.planned_pull_requests["api"].clone();
    hostile_url.publication.draft_pull_requests.insert(
        "api".into(),
        DraftPullRequest {
            repository_id: "api".into(),
            number: 1,
            url: "https://token@example.com/api/pull/1".into(),
            head_branch: plan.head_branch,
            base_branch: plan.base_branch,
            state: PullRequestState::Draft,
        },
    );
    assert_invalid(&hostile_url, &task);

    let mut hostile_approval = changed_change_set(&task);
    hostile_approval.merge_gate = MergeGate::Approved {
        approval: MergeApproval::Human {
            actor: "bad\0actor".into(),
        },
    };
    assert_invalid(&hostile_approval, &task);
}

fn changed_change_set(task: &TaskWorkspace) -> ChangeSet {
    let mut change_set = ChangeSet::for_task(task).unwrap();
    for (repository_id, repository) in &mut change_set.repositories {
        repository.snapshot = if repository_id == "api" {
            RepositorySnapshot::Changed {
                head_commit: "2".repeat(40),
                commits_ahead: 1,
                files: vec![ChangedFile {
                    path: PathBuf::from("src/lib.rs"),
                    kind: ChangedFileKind::Modified,
                    staged: false,
                    worktree: false,
                }],
                insertions: 2,
                deletions: 1,
                diff: DiffSummary {
                    sha256: "b".repeat(64),
                    bytes: 42,
                    truncated: false,
                },
            }
        } else {
            RepositorySnapshot::Unchanged {
                head_commit: "1".repeat(40),
                commits_ahead: 0,
            }
        };
    }
    change_set
}

fn assert_invalid(change_set: &ChangeSet, task: &TaskWorkspace) {
    assert_eq!(
        change_set.validate_for_task(task).unwrap_err().code,
        "invalid_task_change_set_state"
    );
}
