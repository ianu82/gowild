use std::collections::{BTreeMap, BTreeSet};

use super::{ChangeSet, DraftPullRequestPlan, RepositorySnapshot};
use crate::project::ProjectError;

impl ChangeSet {
    /// Creates a draft-only review plan for the affected repositories.
    ///
    /// Base branches are explicit inputs because a commit baseline alone does
    /// not prove which hosting branch should receive a pull request. Publishing
    /// and merging are intentionally outside this operation.
    pub fn plan_draft_pull_requests(
        &mut self,
        base_branches: &BTreeMap<String, String>,
    ) -> Result<&BTreeMap<String, DraftPullRequestPlan>, ProjectError> {
        if !self.publication.draft_pull_requests.is_empty() {
            return Err(ProjectError::new(
                "task_change_set_already_published",
                "a published change set cannot replace its draft pull-request plan",
            ));
        }
        if self
            .repositories
            .values()
            .any(|repository| matches!(repository.snapshot, RepositorySnapshot::Pending))
        {
            return Err(ProjectError::new(
                "task_change_set_not_inspected",
                "every repository must be inspected before planning pull requests",
            ));
        }
        let affected = self
            .affected_repository_ids()
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        if affected.is_empty() {
            return Err(ProjectError::new(
                "task_change_set_has_no_changes",
                "the task has no repository changes to review",
            ));
        }

        let title = review_title(&self.outcome);
        let mut plans = BTreeMap::new();
        for (index, repository_id) in self
            .dependency_order
            .iter()
            .filter(|repository_id| affected.contains(*repository_id))
            .enumerate()
        {
            let repository = &self.repositories[repository_id];
            let head_branch = repository.branch.as_deref().ok_or_else(|| {
                ProjectError::new(
                    "task_change_set_branch_required",
                    format!(
                        "repository '{repository_id}' must activate its task branch before review"
                    ),
                )
            })?;
            let base_branch = base_branches.get(repository_id).ok_or_else(|| {
                ProjectError::new(
                    "task_change_set_base_branch_required",
                    format!("repository '{repository_id}' needs an explicit review base branch"),
                )
            })?;
            validate_review_branch(repository_id, "head", head_branch)?;
            validate_review_branch(repository_id, "base", base_branch)?;
            if head_branch == base_branch {
                return Err(ProjectError::new(
                    "task_change_set_branch_collision",
                    format!(
                        "repository '{repository_id}' task and base branches must be different"
                    ),
                ));
            }
            let depends_on = repository
                .depends_on
                .iter()
                .filter(|dependency| affected.contains(*dependency))
                .cloned()
                .collect::<Vec<_>>();
            let position = u32::try_from(index + 1).map_err(|_| {
                ProjectError::new(
                    "task_change_set_too_many_repositories",
                    "pull-request plan order exceeds its supported range",
                )
            })?;
            plans.insert(
                repository_id.clone(),
                DraftPullRequestPlan {
                    repository_id: repository_id.clone(),
                    position,
                    head_branch: head_branch.to_string(),
                    base_branch: base_branch.clone(),
                    title: title.clone(),
                    body: review_body(self, repository_id, position, &depends_on),
                    depends_on,
                },
            );
        }
        self.publication.planned_pull_requests = plans;
        Ok(&self.publication.planned_pull_requests)
    }
}

fn review_title(outcome: &str) -> String {
    let title = outcome.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = title.chars();
    let shortened = chars.by_ref().take(117).collect::<String>();
    if chars.next().is_some() {
        format!("{shortened}...")
    } else {
        shortened
    }
}

fn review_body(
    change_set: &ChangeSet,
    repository_id: &str,
    position: u32,
    depends_on: &[String],
) -> String {
    let dependencies = if depends_on.is_empty() {
        "None".to_string()
    } else {
        depends_on.join(", ")
    };
    format!(
        "GoWild coordinated change set `{group}`\n\nOutcome\n\n{outcome}\n\nReview context\n\n- Repository: `{repository_id}`\n- Merge position: {position}\n- Affected dependencies: {dependencies}\n\nThis pull request must remain a draft until the coordinated change set is reviewed. Merge is not automated by GoWild.\n",
        group = change_set.publication.group_id,
        outcome = change_set.outcome,
    )
}

fn validate_review_branch(
    repository_id: &str,
    role: &str,
    branch: &str,
) -> Result<(), ProjectError> {
    let safe = !branch.is_empty()
        && branch.len() <= 255
        && !branch.starts_with(['-', '/', '.'])
        && !branch.ends_with(['/', '.'])
        && !branch.contains("..")
        && !branch.contains("//")
        && !branch.contains("@{")
        && branch.split('/').all(|component| {
            !component.is_empty()
                && !component.ends_with(".lock")
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        });
    if safe {
        Ok(())
    } else {
        Err(ProjectError::new(
            "task_change_set_invalid_branch",
            format!("repository '{repository_id}' has an unsafe {role} branch name"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_is_single_line_and_bounded() {
        let title = review_title(&format!("  First line\n{}  ", "é".repeat(200)));
        assert!(!title.contains('\n'));
        assert_eq!(title.chars().count(), 120);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn branch_validation_accepts_normal_paths_and_rejects_git_metacharacters() {
        validate_review_branch("api", "base", "release/2026.08").unwrap();
        for branch in ["-main", "refs//main", "topic..main", "topic@{1}", "x.lock"] {
            assert!(validate_review_branch("api", "base", branch).is_err());
        }
    }
}
