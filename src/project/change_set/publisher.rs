use std::path::Path;

use super::{
    ChangeSet, ChangedFileKind, DraftPullRequest, DraftPullRequestPlan, PullRequestState,
    RepositorySnapshot,
};
use crate::project::ProjectError;

mod system;

#[cfg(test)]
mod system_tests;

pub(crate) use system::GitHubCliDraftPublisher;

/// The complete input for creating or updating one draft review.
pub struct DraftPublicationRequest<'a> {
    pub checkout_path: &'a Path,
    pub plan: &'a DraftPullRequestPlan,
    pub existing: Option<&'a DraftPullRequest>,
}

/// A deliberately draft-only hosting boundary. There is no merge operation.
pub trait DraftPullRequestPublisher {
    fn publish_draft(
        &mut self,
        request: DraftPublicationRequest<'_>,
    ) -> Result<DraftPullRequest, ProjectError>;
}

impl ChangeSet {
    /// Publishes the dependency-ordered review plan and records progress after
    /// each repository so a failed group can be resumed without duplicates.
    pub fn publish_draft_pull_requests(
        &mut self,
        publisher: &mut impl DraftPullRequestPublisher,
    ) -> Result<&std::collections::BTreeMap<String, DraftPullRequest>, ProjectError> {
        self.validate_publication_plan()?;
        let order = self
            .dependency_order
            .iter()
            .filter(|repository_id| {
                self.publication
                    .planned_pull_requests
                    .contains_key(*repository_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        for repository_id in order {
            let repository = &self.repositories[&repository_id];
            require_committed_change(&repository_id, &repository.snapshot)?;
            let plan = &self.publication.planned_pull_requests[&repository_id];
            let existing = self
                .publication
                .draft_pull_requests
                .get(&repository_id)
                .cloned();
            if existing.as_ref().is_some_and(|pull_request| {
                matches!(
                    pull_request.state,
                    PullRequestState::Closed | PullRequestState::Merged
                )
            }) {
                return Err(ProjectError::new(
                    "task_change_set_pull_request_terminal",
                    format!(
                        "repository '{repository_id}' pull request is already closed or merged"
                    ),
                ));
            }
            if existing.as_ref().is_some_and(|pull_request| {
                pull_request.repository_id != plan.repository_id
                    || pull_request.head_branch != plan.head_branch
            }) {
                return Err(ProjectError::new(
                    "task_change_set_pull_request_mismatch",
                    format!(
                        "repository '{repository_id}' published pull request no longer matches its plan"
                    ),
                ));
            }
            let published = publisher.publish_draft(DraftPublicationRequest {
                checkout_path: &repository.checkout_path,
                plan,
                existing: existing.as_ref(),
            })?;
            validate_published_pull_request(plan, existing.is_some(), &published)?;
            self.publication
                .draft_pull_requests
                .insert(repository_id, published);
        }
        Ok(&self.publication.draft_pull_requests)
    }

    fn validate_publication_plan(&self) -> Result<(), ProjectError> {
        if self.publication.planned_pull_requests.is_empty() {
            return Err(ProjectError::new(
                "task_change_set_not_planned",
                "draft pull requests must be planned before publication",
            ));
        }
        let affected = self.affected_repository_ids();
        if affected.len() != self.publication.planned_pull_requests.len()
            || affected.iter().any(|repository_id| {
                !self
                    .publication
                    .planned_pull_requests
                    .contains_key(*repository_id)
            })
        {
            return Err(ProjectError::new(
                "task_change_set_plan_mismatch",
                "draft pull-request plan does not match the affected repositories",
            ));
        }
        Ok(())
    }
}

fn require_committed_change(
    repository_id: &str,
    snapshot: &RepositorySnapshot,
) -> Result<(), ProjectError> {
    let RepositorySnapshot::Changed {
        commits_ahead,
        files,
        ..
    } = snapshot
    else {
        return Err(ProjectError::new(
            "task_change_set_plan_mismatch",
            format!("repository '{repository_id}' has no publishable change"),
        ));
    };
    if files.iter().any(|file| {
        file.staged
            || file.worktree
            || matches!(
                file.kind,
                ChangedFileKind::Unmerged | ChangedFileKind::Untracked
            )
    }) {
        return Err(ProjectError::new(
            "task_change_set_uncommitted_changes",
            format!(
                "repository '{repository_id}' must commit or discard working changes before publication"
            ),
        ));
    }
    if *commits_ahead == 0 {
        return Err(ProjectError::new(
            "task_change_set_commit_required",
            format!("repository '{repository_id}' has no task commit to publish"),
        ));
    }
    Ok(())
}

fn validate_published_pull_request(
    plan: &DraftPullRequestPlan,
    updating: bool,
    pull_request: &DraftPullRequest,
) -> Result<(), ProjectError> {
    let url = url::Url::parse(&pull_request.url).ok();
    let valid_url = url.as_ref().is_some_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    });
    let valid_state = if updating {
        matches!(
            pull_request.state,
            PullRequestState::Draft | PullRequestState::Open
        )
    } else {
        pull_request.state == PullRequestState::Draft
    };
    if pull_request.repository_id == plan.repository_id
        && pull_request.number > 0
        && pull_request.head_branch == plan.head_branch
        && pull_request.base_branch == plan.base_branch
        && valid_url
        && valid_state
    {
        Ok(())
    } else {
        Err(ProjectError::new(
            "task_change_set_invalid_pull_request",
            format!(
                "repository '{}' publisher returned an invalid draft pull request",
                plan.repository_id
            ),
        ))
    }
}
