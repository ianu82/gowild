use std::collections::BTreeSet;

use super::collector::{
    validate_relative_git_path, DIFF_DISPLAY_BYTES, MAX_CHANGED_FILES, MAX_DIFF_BYTES,
};
use super::planner::validate_review_branch;
use super::{
    dependency_order, ChangeSet, CheckStatus, MergeApproval, MergeGate, PullRequestState,
    RepositorySnapshot, CHANGE_SET_VERSION,
};
use crate::project::task_workspace::TaskWorkspace;
use crate::project::task_workspace::{
    validate_digest, validate_git_object_id, validate_identifier,
};
use crate::project::ProjectError;

const MAX_CHECKS: usize = 10_000;
const MAX_REVIEW_BODY_BYTES: usize = 64 * 1024;
const MAX_APPROVAL_TEXT_BYTES: usize = 4 * 1024;

impl ChangeSet {
    /// Rejects tampered or stale-future state before a persisted/API value can
    /// influence Git, test or hosting operations.
    pub fn validate_for_task(&self, task: &TaskWorkspace) -> Result<(), ProjectError> {
        task.validate_integrity()?;
        if self.schema_version != CHANGE_SET_VERSION
            || self.project_id != task.project_id
            || self.task_id != task.id
            || self.task_revision > task.revision
            || self.manifest_digest != task.manifest_digest
            || self.outcome != task.outcome
            || self.dependency_order != dependency_order(task)?
            || self.repositories.len() != task.repositories.len()
            || self.publication.group_id != format!("{}:{}", task.project_id, task.id)
        {
            return Err(invalid_state());
        }

        for (repository_id, task_repository) in &task.repositories {
            let repository = self
                .repositories
                .get(repository_id)
                .ok_or_else(invalid_state)?;
            let expected_branch = task.branch_name(repository_id);
            if repository.repository_id != *repository_id
                || repository.checkout_path != task.repository_checkout_path(repository_id)
                || repository.base_commit != task_repository.base_commit
                || repository.depends_on != task_repository.depends_on
                || repository
                    .branch
                    .as_ref()
                    .is_some_and(|branch| branch != &expected_branch)
            {
                return Err(invalid_state());
            }
            validate_snapshot(repository_id, &repository.snapshot)?;
        }
        self.validate_checks()?;
        self.validate_publication()?;
        validate_merge_gate(&self.merge_gate)
    }

    pub fn is_stale_for_task(&self, task: &TaskWorkspace) -> bool {
        self.task_revision != task.revision
    }

    fn validate_checks(&self) -> Result<(), ProjectError> {
        if self.checks.len() > MAX_CHECKS {
            return Err(invalid_state());
        }
        for (command_id, check) in &self.checks {
            validate_identifier("change-set check id", command_id).map_err(|_| invalid_state())?;
            if check.command_id != *command_id
                || check
                    .repository_id
                    .as_ref()
                    .is_some_and(|repository_id| !self.repositories.contains_key(repository_id))
                || check
                    .failure_code
                    .as_ref()
                    .is_some_and(|code| validate_identifier("failure code", code).is_err())
                || (check.status == CheckStatus::Passed
                    && (check.exit_code != Some(0) || check.failure_code.is_some()))
                || (check.status == CheckStatus::Pending
                    && (check.duration_ms.is_some()
                        || check.exit_code.is_some()
                        || check.failure_code.is_some()))
                || (check.status == CheckStatus::Skipped
                    && (check.exit_code.is_some() || check.failure_code.is_some()))
            {
                return Err(invalid_state());
            }
        }
        Ok(())
    }

    fn validate_publication(&self) -> Result<(), ProjectError> {
        if !self.publication.planned_pull_requests.is_empty() {
            self.validate_publication_plan()
                .map_err(|_| invalid_state())?;
        } else if !self.publication.draft_pull_requests.is_empty() {
            return Err(invalid_state());
        }

        let affected = self
            .affected_repository_ids()
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut expected_position = 1_u32;
        for repository_id in &self.dependency_order {
            let Some(plan) = self.publication.planned_pull_requests.get(repository_id) else {
                continue;
            };
            let repository = &self.repositories[repository_id];
            let expected_dependencies = repository
                .depends_on
                .iter()
                .filter(|dependency| affected.contains(dependency.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            if plan.repository_id != *repository_id
                || plan.position != expected_position
                || repository.branch.as_deref() != Some(plan.head_branch.as_str())
                || plan.head_branch == plan.base_branch
                || plan.depends_on != expected_dependencies
                || !valid_review_title(&plan.title)
                || !valid_text(&plan.body, MAX_REVIEW_BODY_BYTES)
                || validate_review_branch(repository_id, "head", &plan.head_branch).is_err()
                || validate_review_branch(repository_id, "base", &plan.base_branch).is_err()
            {
                return Err(invalid_state());
            }
            expected_position = expected_position.checked_add(1).ok_or_else(invalid_state)?;
        }

        for (repository_id, pull_request) in &self.publication.draft_pull_requests {
            let plan = self
                .publication
                .planned_pull_requests
                .get(repository_id)
                .ok_or_else(invalid_state)?;
            let url = url::Url::parse(&pull_request.url).map_err(|_| invalid_state())?;
            if pull_request.repository_id != *repository_id
                || pull_request.number == 0
                || pull_request.head_branch != plan.head_branch
                || pull_request.base_branch != plan.base_branch
                || url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || !matches!(
                    pull_request.state,
                    PullRequestState::Draft
                        | PullRequestState::Open
                        | PullRequestState::Closed
                        | PullRequestState::Merged
                )
            {
                return Err(invalid_state());
            }
        }
        Ok(())
    }
}

fn validate_snapshot(
    repository_id: &str,
    snapshot: &RepositorySnapshot,
) -> Result<(), ProjectError> {
    match snapshot {
        RepositorySnapshot::Pending => Ok(()),
        RepositorySnapshot::Unchanged { head_commit, .. } => {
            validate_git_object_id(head_commit).map_err(|_| invalid_state())
        }
        RepositorySnapshot::Changed {
            head_commit,
            files,
            diff,
            ..
        } => {
            validate_git_object_id(head_commit).map_err(|_| invalid_state())?;
            validate_digest("change-set diff", &diff.sha256, 64).map_err(|_| invalid_state())?;
            if files.len() > MAX_CHANGED_FILES
                || diff.bytes > MAX_DIFF_BYTES
                || diff.truncated != (diff.bytes > DIFF_DISPLAY_BYTES)
            {
                return Err(invalid_state());
            }
            let mut unique_paths = BTreeSet::new();
            for file in files {
                validate_relative_git_path(repository_id, &file.path)
                    .map_err(|_| invalid_state())?;
                if !unique_paths.insert(&file.path) {
                    return Err(invalid_state());
                }
            }
            Ok(())
        }
    }
}

fn validate_merge_gate(gate: &MergeGate) -> Result<(), ProjectError> {
    let valid = match gate {
        MergeGate::AwaitingApproval => true,
        MergeGate::Approved {
            approval: MergeApproval::Human { actor },
        } => valid_text(actor, 255),
        MergeGate::Approved {
            approval:
                MergeApproval::Policy {
                    policy_id,
                    evidence,
                },
        } => {
            validate_identifier("merge policy", policy_id).is_ok()
                && valid_text(evidence, MAX_APPROVAL_TEXT_BYTES)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(invalid_state())
    }
}

fn valid_review_title(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 120
        && !value.chars().any(char::is_control)
        && value.trim() == value
}

fn valid_text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= limit
        && !value.chars().any(|character| {
            character == '\0'
                || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        })
}

fn invalid_state() -> ProjectError {
    ProjectError::new(
        "invalid_task_change_set_state",
        "task change-set state is invalid or does not match its task workspace",
    )
}
