#![allow(
    dead_code,
    reason = "change-set collectors and API consumers land in the next stacked changes"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::task_workspace::{TaskWorkspace, TaskWorkspacePhase};
use super::ProjectError;

mod collector;

#[cfg(test)]
mod collector_tests;
#[cfg(test)]
mod tests;

pub const CHANGE_SET_VERSION: u32 = 1;

/// One coordinated, reviewable outcome across every repository in a task.
///
/// Repository facts and checks are populated by collectors. Publication is a
/// separate, explicit step, and merge approval is represented but never
/// inferred from the existence or state of a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSet {
    pub schema_version: u32,
    pub project_id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub manifest_digest: String,
    pub outcome: String,
    pub dependency_order: Vec<String>,
    pub repositories: BTreeMap<String, RepositoryChange>,
    pub checks: BTreeMap<String, ChangeSetCheck>,
    pub publication: ChangeSetPublication,
    pub merge_gate: MergeGate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryChange {
    pub repository_id: String,
    pub checkout_path: PathBuf,
    pub base_commit: String,
    pub branch: Option<String>,
    pub depends_on: Vec<String>,
    pub snapshot: RepositorySnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum RepositorySnapshot {
    Pending,
    Unchanged {
        head_commit: String,
        commits_ahead: u64,
    },
    Changed {
        head_commit: String,
        commits_ahead: u64,
        files: Vec<ChangedFile>,
        insertions: u64,
        deletions: u64,
        diff: DiffSummary,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub kind: ChangedFileKind,
    pub staged: bool,
    pub worktree: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangedFileKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Unmerged,
    Untracked,
}

/// Bounded patch metadata. The patch itself is collected on demand so task
/// state does not become a second, long-lived copy of source or secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffSummary {
    pub sha256: String,
    pub bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSetCheck {
    pub command_id: String,
    pub repository_id: Option<String>,
    pub status: CheckStatus,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pending,
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSetPublication {
    pub group_id: String,
    pub draft_pull_requests: BTreeMap<String, DraftPullRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftPullRequest {
    pub repository_id: String,
    pub number: u64,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub state: PullRequestState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Draft,
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MergeGate {
    AwaitingApproval,
    Approved { approval: MergeApproval },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MergeApproval {
    Human { actor: String },
    Policy { policy_id: String, evidence: String },
}

impl ChangeSet {
    pub fn for_task(task: &TaskWorkspace) -> Result<Self, ProjectError> {
        task.validate_integrity()?;
        if matches!(
            task.phase,
            TaskWorkspacePhase::Planned
                | TaskWorkspacePhase::Provisioning
                | TaskWorkspacePhase::Cleaning
                | TaskWorkspacePhase::Cleaned
        ) {
            return Err(ProjectError::new(
                "task_change_set_unavailable",
                "a change set requires a provisioned task workspace",
            ));
        }

        let dependency_order = dependency_order(task)?;
        let repositories = task
            .repositories
            .iter()
            .map(|(repository_id, repository)| {
                let worktree = repository.worktree.as_ref().ok_or_else(|| {
                    ProjectError::new(
                        "task_change_set_unavailable",
                        format!("repository '{repository_id}' has no provisioned task checkout"),
                    )
                })?;
                Ok((
                    repository_id.clone(),
                    RepositoryChange {
                        repository_id: repository_id.clone(),
                        checkout_path: worktree.checkout_path.clone(),
                        base_commit: repository.base_commit.clone(),
                        branch: worktree.branch.clone(),
                        depends_on: repository.depends_on.clone(),
                        snapshot: RepositorySnapshot::Pending,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, ProjectError>>()?;
        Ok(Self {
            schema_version: CHANGE_SET_VERSION,
            project_id: task.project_id.clone(),
            task_id: task.id.clone(),
            task_revision: task.revision,
            manifest_digest: task.manifest_digest.clone(),
            outcome: task.outcome.clone(),
            dependency_order,
            repositories,
            checks: BTreeMap::new(),
            publication: ChangeSetPublication {
                group_id: format!("{}:{}", task.project_id, task.id),
                draft_pull_requests: BTreeMap::new(),
            },
            merge_gate: MergeGate::AwaitingApproval,
        })
    }

    pub fn affected_repository_ids(&self) -> Vec<&str> {
        self.dependency_order
            .iter()
            .filter(|repository_id| {
                self.repositories.get(*repository_id).is_some_and(|change| {
                    matches!(change.snapshot, RepositorySnapshot::Changed { .. })
                })
            })
            .map(String::as_str)
            .collect()
    }

    pub fn merge_order(&self) -> Vec<&str> {
        self.affected_repository_ids()
    }

    pub fn merge_is_approved(&self) -> bool {
        matches!(self.merge_gate, MergeGate::Approved { .. })
    }
}

fn dependency_order(task: &TaskWorkspace) -> Result<Vec<String>, ProjectError> {
    let mut remaining = task
        .repositories
        .iter()
        .map(|(repository_id, repository)| {
            (
                repository_id.clone(),
                repository
                    .depends_on
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut resolved = BTreeSet::new();
    let mut ordered = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .filter(|(_, dependencies)| dependencies.is_subset(&resolved))
            .map(|(repository_id, _)| repository_id.clone())
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Err(ProjectError::new(
                "task_change_set_dependency_cycle",
                "task repository dependency order could not be resolved",
            ));
        }
        for repository_id in ready {
            remaining.remove(&repository_id);
            resolved.insert(repository_id.clone());
            ordered.push(repository_id);
        }
    }
    Ok(ordered)
}
