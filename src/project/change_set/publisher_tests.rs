use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use super::*;
use crate::project::task_workspace::provision::TaskWorkspaceProvisioner;
use crate::project::task_workspace::provision_tests::ProjectFixture;

#[derive(Default)]
struct FakePublisher {
    calls: Vec<(String, bool)>,
    fail_on: Option<String>,
}

impl DraftPullRequestPublisher for FakePublisher {
    fn publish_draft(
        &mut self,
        request: DraftPublicationRequest<'_>,
    ) -> Result<DraftPullRequest, crate::project::ProjectError> {
        let repository_id = request.plan.repository_id.clone();
        self.calls
            .push((repository_id.clone(), request.existing.is_some()));
        if self.fail_on.as_deref() == Some(repository_id.as_str()) {
            return Err(crate::project::ProjectError::new(
                "fixture_publish_failed",
                "fixture publisher failed",
            ));
        }
        Ok(DraftPullRequest {
            repository_id,
            number: request
                .existing
                .map_or(100 + u64::from(request.plan.position), |pull| pull.number),
            url: format!(
                "https://github.com/example/{}/pull/{}",
                request.plan.repository_id,
                100 + u64::from(request.plan.position)
            ),
            head_branch: request.plan.head_branch.clone(),
            base_branch: request.plan.base_branch.clone(),
            state: PullRequestState::Draft,
        })
    }
}

#[test]
fn publication_creates_then_updates_drafts_in_dependency_order() {
    let mut change_set = publishable_change_set("publish");
    let mut publisher = FakePublisher::default();

    let created = change_set
        .publish_draft_pull_requests(&mut publisher)
        .unwrap()
        .clone();
    let updated = change_set
        .publish_draft_pull_requests(&mut publisher)
        .unwrap()
        .clone();

    assert_eq!(created.len(), 2);
    assert_eq!(created, updated);
    assert_eq!(publisher.calls[0], ("api".into(), false));
    assert_eq!(publisher.calls[1], ("web".into(), false));
    assert_eq!(publisher.calls[2], ("api".into(), true));
    assert_eq!(publisher.calls[3], ("web".into(), true));
    assert!(created
        .values()
        .all(|pull_request| pull_request.state == PullRequestState::Draft));
    assert!(!change_set.merge_is_approved());
}

#[test]
fn publication_records_partial_progress_and_resumes_without_duplicate_creation() {
    let mut change_set = publishable_change_set("resume");
    let mut publisher = FakePublisher {
        calls: Vec::new(),
        fail_on: Some("web".into()),
    };

    let error = change_set
        .publish_draft_pull_requests(&mut publisher)
        .unwrap_err();
    assert_eq!(error.code, "fixture_publish_failed");
    assert_eq!(
        change_set
            .publication
            .draft_pull_requests
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["api"]
    );

    publisher.fail_on = None;
    change_set
        .publish_draft_pull_requests(&mut publisher)
        .unwrap();
    assert_eq!(publisher.calls[2], ("api".into(), true));
    assert_eq!(publisher.calls[3], ("web".into(), false));
    assert_eq!(change_set.publication.draft_pull_requests.len(), 2);
}

#[test]
fn publication_refuses_uncommitted_or_invalid_publisher_results() {
    let (fixture, task, _) = planned_change_set("blocked", false);
    let api = &task.repositories["api"]
        .worktree
        .as_ref()
        .unwrap()
        .checkout_path;
    std::fs::write(api.join("still-dirty.txt"), b"dirty\n").unwrap();
    let mut change_set = fixture
        .states
        .inspect_change_set(&fixture.project, "blocked")
        .unwrap();
    change_set
        .plan_draft_pull_requests(&base_branches())
        .unwrap();
    let mut publisher = FakePublisher::default();
    let error = change_set
        .publish_draft_pull_requests(&mut publisher)
        .unwrap_err();
    assert_eq!(error.code, "task_change_set_uncommitted_changes");
    assert!(publisher.calls.is_empty());

    let mut publishable = publishable_change_set("invalid");
    let mut invalid = InvalidPublisher;
    let error = publishable
        .publish_draft_pull_requests(&mut invalid)
        .unwrap_err();
    assert_eq!(error.code, "task_change_set_invalid_pull_request");
    assert!(publishable.publication.draft_pull_requests.is_empty());
}

struct InvalidPublisher;

impl DraftPullRequestPublisher for InvalidPublisher {
    fn publish_draft(
        &mut self,
        request: DraftPublicationRequest<'_>,
    ) -> Result<DraftPullRequest, crate::project::ProjectError> {
        Ok(DraftPullRequest {
            repository_id: request.plan.repository_id.clone(),
            number: 0,
            url: "http://unsafe.invalid/1".into(),
            head_branch: request.plan.head_branch.clone(),
            base_branch: request.plan.base_branch.clone(),
            state: PullRequestState::Open,
        })
    }
}

fn publishable_change_set(task_id: &str) -> ChangeSet {
    planned_change_set(task_id, true).2
}

fn planned_change_set(
    task_id: &str,
    commit: bool,
) -> (
    ProjectFixture,
    crate::project::task_workspace::TaskWorkspace,
    ChangeSet,
) {
    let fixture = ProjectFixture::new(false);
    fixture.create_task(task_id);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            task_id,
        )
        .unwrap();
    for repository_id in ["api", "web"] {
        provisioner
            .activate_repository(
                &fixture.definition,
                &fixture.private_state,
                &fixture.project,
                task_id,
                repository_id,
            )
            .unwrap();
        let checkout = &task.repositories[repository_id]
            .worktree
            .as_ref()
            .unwrap()
            .checkout_path;
        std::fs::write(
            checkout.join(format!("{repository_id}.txt")),
            format!("{repository_id}\n"),
        )
        .unwrap();
        if commit {
            run_git(checkout, &["add", "."]);
            run_git(checkout, &["commit", "-m", "task change"]);
        }
    }
    let mut change_set = fixture
        .states
        .inspect_change_set(&fixture.project, task_id)
        .unwrap();
    change_set
        .plan_draft_pull_requests(&base_branches())
        .unwrap();
    (fixture, task, change_set)
}

fn base_branches() -> BTreeMap<String, String> {
    BTreeMap::from([("api".into(), "main".into()), ("web".into(), "main".into())])
}

fn run_git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}
