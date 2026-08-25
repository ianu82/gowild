use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{DraftPublicationRequest, DraftPullRequestPublisher};
use crate::project::change_set::{DraftPullRequest, PullRequestState};
use crate::project::ProjectError;

mod process;
mod remote;

use process::run_checked;
use remote::{repository_selector_from_remote, validate_remote_name, validate_repository_selector};

/// Noninteractive Git + GitHub CLI adapter for draft review publication.
///
/// It can push exact task branches and create, edit, list or view pull
/// requests. It intentionally has no merge path and never force-pushes.
pub struct GitHubCliDraftPublisher {
    remote: String,
    git_program: PathBuf,
    gh_program: PathBuf,
    repository_override: Option<String>,
    environment: BTreeMap<OsString, OsString>,
}

impl GitHubCliDraftPublisher {
    pub fn new(remote: impl Into<String>) -> Result<Self, ProjectError> {
        let remote = remote.into();
        validate_remote_name(&remote)?;
        Ok(Self {
            remote,
            git_program: PathBuf::from("git"),
            gh_program: PathBuf::from("gh"),
            repository_override: None,
            environment: BTreeMap::new(),
        })
    }

    #[cfg(test)]
    pub(super) fn for_test(
        remote: impl Into<String>,
        git_program: PathBuf,
        gh_program: PathBuf,
        repository: impl Into<String>,
        environment: BTreeMap<OsString, OsString>,
    ) -> Result<Self, ProjectError> {
        let mut publisher = Self::new(remote)?;
        publisher.git_program = git_program;
        publisher.gh_program = gh_program;
        publisher.repository_override = Some(repository.into());
        publisher.environment = environment;
        Ok(publisher)
    }

    fn verify_and_push(&self, checkout: &Path, head_branch: &str) -> Result<(), ProjectError> {
        let branch = self.git_text(
            checkout,
            ["symbolic-ref", "--quiet", "--short", "HEAD"],
            "inspect task branch",
        )?;
        if branch != head_branch {
            return Err(ProjectError::new(
                "task_change_set_branch_mismatch",
                "task checkout is no longer on its planned publication branch",
            ));
        }
        let status = self.git(
            checkout,
            ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            None,
            "inspect working tree",
        )?;
        if !status.is_empty() {
            return Err(ProjectError::new(
                "task_change_set_uncommitted_changes",
                "task checkout changed after review planning and must be committed again",
            ));
        }
        let refspec = format!("refs/heads/{head_branch}:refs/heads/{head_branch}");
        self.git(
            checkout,
            [
                "push",
                "--porcelain",
                "--set-upstream",
                "--",
                self.remote.as_str(),
                refspec.as_str(),
            ],
            None,
            "push task branch",
        )?;
        Ok(())
    }

    fn repository(&self, checkout: &Path) -> Result<String, ProjectError> {
        if let Some(repository) = &self.repository_override {
            validate_repository_selector(repository)?;
            return Ok(repository.clone());
        }
        let remote_url = self.git_text(
            checkout,
            ["remote", "get-url", "--", self.remote.as_str()],
            "resolve GitHub remote",
        )?;
        repository_selector_from_remote(&remote_url)
    }

    fn list_pull_requests(
        &self,
        checkout: &Path,
        repository: &str,
        head_branch: &str,
    ) -> Result<Vec<GhPullRequest>, ProjectError> {
        let output = self.gh(
            checkout,
            [
                "pr",
                "list",
                "--repo",
                repository,
                "--head",
                head_branch,
                "--state",
                "all",
                "--limit",
                "20",
                "--json",
                "number,url,state,isDraft,baseRefName,headRefName,mergedAt",
            ],
            None,
            "list pull requests",
        )?;
        serde_json::from_slice(&output).map_err(|_| invalid_gh_output())
    }

    fn create(
        &self,
        checkout: &Path,
        repository: &str,
        request: DraftPublicationRequest<'_>,
    ) -> Result<(), ProjectError> {
        self.gh(
            checkout,
            [
                "pr",
                "create",
                "--repo",
                repository,
                "--draft",
                "--base",
                request.plan.base_branch.as_str(),
                "--head",
                request.plan.head_branch.as_str(),
                "--title",
                request.plan.title.as_str(),
                "--body-file",
                "-",
            ],
            Some(request.plan.body.as_bytes()),
            "create draft pull request",
        )?;
        Ok(())
    }

    fn edit(
        &self,
        checkout: &Path,
        repository: &str,
        request: DraftPublicationRequest<'_>,
        number: u64,
    ) -> Result<(), ProjectError> {
        let number = number.to_string();
        self.gh(
            checkout,
            [
                "pr",
                "edit",
                number.as_str(),
                "--repo",
                repository,
                "--base",
                request.plan.base_branch.as_str(),
                "--title",
                request.plan.title.as_str(),
                "--body-file",
                "-",
            ],
            Some(request.plan.body.as_bytes()),
            "update draft pull request",
        )?;
        Ok(())
    }

    fn git_text<const N: usize>(
        &self,
        checkout: &Path,
        args: [&str; N],
        operation: &'static str,
    ) -> Result<String, ProjectError> {
        let output = self.git(checkout, args, None, operation)?;
        String::from_utf8(output)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(invalid_git_output)
    }

    fn git<const N: usize>(
        &self,
        checkout: &Path,
        args: [&str; N],
        stdin: Option<&[u8]>,
        operation: &'static str,
    ) -> Result<Vec<u8>, ProjectError> {
        run_checked(
            &self.git_program,
            checkout,
            args,
            stdin,
            operation,
            &self.environment,
        )
    }

    fn gh<const N: usize>(
        &self,
        checkout: &Path,
        args: [&str; N],
        stdin: Option<&[u8]>,
        operation: &'static str,
    ) -> Result<Vec<u8>, ProjectError> {
        run_checked(
            &self.gh_program,
            checkout,
            args,
            stdin,
            operation,
            &self.environment,
        )
    }
}

impl DraftPullRequestPublisher for GitHubCliDraftPublisher {
    fn publish_draft(
        &mut self,
        request: DraftPublicationRequest<'_>,
    ) -> Result<DraftPullRequest, ProjectError> {
        self.verify_and_push(request.checkout_path, &request.plan.head_branch)?;
        let repository = self.repository(request.checkout_path)?;
        let before = self.list_pull_requests(
            request.checkout_path,
            &repository,
            &request.plan.head_branch,
        )?;
        let existing = select_existing(request.existing, &before)?;
        let number = if let Some(existing) = existing {
            if request.existing.is_none() && existing.state()? != PullRequestState::Draft {
                return Err(ProjectError::new(
                    "task_change_set_existing_pr_not_draft",
                    "an existing pull request for the task branch is not a draft",
                ));
            }
            self.edit(
                request.checkout_path,
                &repository,
                DraftPublicationRequest {
                    checkout_path: request.checkout_path,
                    plan: request.plan,
                    existing: request.existing,
                },
                existing.number,
            )?;
            existing.number
        } else {
            self.create(
                request.checkout_path,
                &repository,
                DraftPublicationRequest {
                    checkout_path: request.checkout_path,
                    plan: request.plan,
                    existing: request.existing,
                },
            )?;
            let after = self.list_pull_requests(
                request.checkout_path,
                &repository,
                &request.plan.head_branch,
            )?;
            let created = select_active(&after)?.ok_or_else(|| {
                ProjectError::new(
                    "task_change_set_pull_request_missing",
                    "GitHub did not return the created draft pull request",
                )
            })?;
            if created.state()? != PullRequestState::Draft {
                return Err(ProjectError::new(
                    "task_change_set_existing_pr_not_draft",
                    "GitHub did not create the pull request as a draft",
                ));
            }
            created.number
        };
        let after = self.list_pull_requests(
            request.checkout_path,
            &repository,
            &request.plan.head_branch,
        )?;
        let published = after
            .iter()
            .find(|pull_request| pull_request.number == number)
            .ok_or_else(|| {
                ProjectError::new(
                    "task_change_set_pull_request_missing",
                    "GitHub did not return the published pull request",
                )
            })?;
        published.to_domain(&request.plan.repository_id)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct GhPullRequest {
    number: u64,
    url: String,
    state: String,
    is_draft: bool,
    base_ref_name: String,
    head_ref_name: String,
    merged_at: Option<String>,
}

impl GhPullRequest {
    fn state(&self) -> Result<PullRequestState, ProjectError> {
        if self.merged_at.is_some() || self.state.eq_ignore_ascii_case("merged") {
            Ok(PullRequestState::Merged)
        } else if self.state.eq_ignore_ascii_case("closed") {
            Ok(PullRequestState::Closed)
        } else if self.state.eq_ignore_ascii_case("open") && self.is_draft {
            Ok(PullRequestState::Draft)
        } else if self.state.eq_ignore_ascii_case("open") {
            Ok(PullRequestState::Open)
        } else {
            Err(invalid_gh_output())
        }
    }

    fn to_domain(&self, repository_id: &str) -> Result<DraftPullRequest, ProjectError> {
        Ok(DraftPullRequest {
            repository_id: repository_id.to_string(),
            number: self.number,
            url: self.url.clone(),
            head_branch: self.head_ref_name.clone(),
            base_branch: self.base_ref_name.clone(),
            state: self.state()?,
        })
    }
}

fn select_existing<'a>(
    recorded: Option<&DraftPullRequest>,
    pull_requests: &'a [GhPullRequest],
) -> Result<Option<&'a GhPullRequest>, ProjectError> {
    if let Some(recorded) = recorded {
        let pull_request = pull_requests
            .iter()
            .find(|pull_request| pull_request.number == recorded.number)
            .ok_or_else(|| {
                ProjectError::new(
                    "task_change_set_pull_request_missing",
                    "recorded pull request no longer exists on GitHub",
                )
            })?;
        if matches!(
            pull_request.state()?,
            PullRequestState::Closed | PullRequestState::Merged
        ) {
            return Err(ProjectError::new(
                "task_change_set_pull_request_terminal",
                "recorded pull request is already closed or merged",
            ));
        }
        return Ok(Some(pull_request));
    }
    select_active(pull_requests)
}

fn select_active(pull_requests: &[GhPullRequest]) -> Result<Option<&GhPullRequest>, ProjectError> {
    let active = pull_requests
        .iter()
        .map(|pull_request| Ok((pull_request, pull_request.state()?)))
        .collect::<Result<Vec<_>, ProjectError>>()?
        .into_iter()
        .filter_map(|(pull_request, state)| {
            matches!(state, PullRequestState::Draft | PullRequestState::Open)
                .then_some(pull_request)
        })
        .collect::<Vec<_>>();
    if active.len() > 1 {
        Err(ProjectError::new(
            "task_change_set_pull_request_ambiguous",
            "multiple active pull requests use the planned task branch",
        ))
    } else {
        Ok(active.first().copied())
    }
}

fn invalid_git_output() -> ProjectError {
    ProjectError::new(
        "task_change_set_publication_git_invalid_output",
        "Git returned invalid publication state",
    )
}

fn invalid_gh_output() -> ProjectError {
    ProjectError::new(
        "task_change_set_publication_github_invalid_output",
        "GitHub CLI returned invalid pull-request state",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_pull_request_selection_rejects_invalid_or_ambiguous_state() {
        let invalid = pull_request(1, "UNKNOWN", false, None);
        assert_eq!(
            select_active(&[invalid]).unwrap_err().code,
            "task_change_set_publication_github_invalid_output"
        );

        let first = pull_request(1, "OPEN", true, None);
        let second = pull_request(2, "OPEN", false, None);
        assert_eq!(
            select_active(&[first, second]).unwrap_err().code,
            "task_change_set_pull_request_ambiguous"
        );
    }

    #[test]
    fn terminal_history_does_not_hide_one_active_draft() {
        let closed = pull_request(1, "CLOSED", false, None);
        let merged = pull_request(2, "CLOSED", false, Some("2026-08-25T00:00:00Z"));
        let draft = pull_request(3, "OPEN", true, None);

        assert_eq!(
            select_active(&[closed, merged, draft])
                .unwrap()
                .unwrap()
                .number,
            3
        );
    }

    fn pull_request(
        number: u64,
        state: &str,
        is_draft: bool,
        merged_at: Option<&str>,
    ) -> GhPullRequest {
        GhPullRequest {
            number,
            url: format!("https://github.com/example/api/pull/{number}"),
            state: state.into(),
            is_draft,
            base_ref_name: "main".into(),
            head_ref_name: "gowild/task/api".into(),
            merged_at: merged_at.map(str::to_string),
        }
    }
}
