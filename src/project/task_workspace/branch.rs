use std::path::Path;

use super::provision::{
    git_stdout, require_matching_definition, task_worktree_entry, verify_detached_task_worktree,
    TaskWorkspaceProvisioner,
};
use super::{
    LoadedProject, OwnedResource, TaskTransitionOperation, TaskTransitionState, TaskWorkspace,
    TaskWorkspacePhase,
};
use crate::project::{ProjectDefinition, ProjectError, ProjectPrivateState};

impl TaskWorkspaceProvisioner<'_> {
    /// Lazily gives one already-provisioned repository its task-owned branch.
    /// Untouched repositories remain detached at their recorded base commits.
    pub fn activate_repository(
        &self,
        definition: &ProjectDefinition,
        private_state: &ProjectPrivateState,
        project: &LoadedProject,
        task_id: &str,
        repository_id: &str,
    ) -> Result<TaskWorkspace, ProjectError> {
        require_matching_definition(definition, project)?;
        private_state.require_execution_trust(definition)?;
        let _task_lock = self.states().lock_task_operations(task_id)?;
        let mut task = self.states().load(task_id)?;
        task.validate(project)?;

        let base_commit = {
            let repository = task.repositories.get(repository_id).ok_or_else(|| {
                ProjectError::new(
                    "unknown_task_workspace_repository",
                    format!("project has no repository '{repository_id}'"),
                )
            })?;
            if repository.worktree.is_none() {
                return Err(ProjectError::new(
                    "task_workspace_worktree_not_ready",
                    format!("repository '{repository_id}' has no materialized worktree"),
                ));
            }
            repository.base_commit.clone()
        };
        let recovering_attention = task.phase == TaskWorkspacePhase::NeedsAttention;
        match task.phase {
            TaskWorkspacePhase::Ready
            | TaskWorkspacePhase::Running
            | TaskWorkspacePhase::Stopped => {}
            TaskWorkspacePhase::NeedsAttention => {
                self.transition_phase(&mut task, TaskWorkspacePhase::Provisioning)?;
            }
            TaskWorkspacePhase::Planned | TaskWorkspacePhase::Provisioning => {
                return Err(ProjectError::new(
                    "task_workspace_not_ready",
                    "task repositories must be provisioned before a branch can be activated",
                ));
            }
            TaskWorkspacePhase::Cleaning | TaskWorkspacePhase::Cleaned => {
                return Err(ProjectError::new(
                    "task_workspace_not_activatable",
                    "a cleaning or cleaned task workspace cannot activate repositories",
                ));
            }
        }
        let resource = OwnedResource::RepositoryBranch {
            repository_id: repository_id.to_string(),
            checkout_path: task.repository_checkout_path(repository_id),
            branch: task.branch_name(repository_id),
            base_commit,
        };
        let recovering_planned_branch = task.journal.iter().any(|transition| {
            transition.operation == TaskTransitionOperation::Acquire
                && transition.state == TaskTransitionState::Planned
                && transition.resource == resource
        });
        let snapshot = task.clone();
        let _repository_lock = self.states().lock_repository_operations(repository_id)?;
        self.ensure_acquired(
            &mut task,
            resource,
            || verify_task_branch(&snapshot, repository_id),
            || ensure_task_branch(&snapshot, repository_id, recovering_planned_branch),
        )?;
        verify_task_branch(&task, repository_id)?;

        if recovering_attention {
            self.transition_phase(&mut task, TaskWorkspacePhase::Ready)?;
        }
        Ok(task)
    }
}

pub(super) fn ensure_task_branch(
    task: &TaskWorkspace,
    repository_id: &str,
    reconcile_planned: bool,
) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let checkout = task.repository_checkout_path(repository_id);
    let branch = task.branch_name(repository_id);
    let entry = task_worktree_entry(&repository.source_path, &checkout)?.ok_or_else(|| {
        ProjectError::new(
            "task_repository_worktree_missing",
            format!("repository '{repository_id}' task worktree is missing"),
        )
    })?;
    if entry.is_bare || entry.is_prunable {
        return Err(branch_mismatch(repository_id));
    }
    if entry
        .branch
        .as_deref()
        .is_some_and(|current| current != branch)
    {
        return Err(ProjectError::new(
            "task_repository_branch_conflict",
            format!("repository '{repository_id}' task worktree is on another branch"),
        ));
    }
    if branch_is_checked_out_elsewhere(&repository.source_path, &checkout, &branch)? {
        return Err(ProjectError::new(
            "task_repository_branch_conflict",
            format!("repository '{repository_id}' task branch is checked out elsewhere"),
        ));
    }

    let existing_commit = branch_commit(repository_id, &repository.source_path, &branch)?;
    if existing_commit.is_some() && !reconcile_planned {
        return Err(ProjectError::new(
            "task_repository_branch_conflict",
            format!("repository '{repository_id}' task branch already exists without ownership"),
        ));
    }
    if existing_commit
        .as_deref()
        .is_some_and(|commit| commit != repository.base_commit)
    {
        return Err(ProjectError::new(
            "task_repository_branch_conflict",
            format!("repository '{repository_id}' task branch no longer matches its base"),
        ));
    }

    match entry.branch.as_deref() {
        Some(current) if current == branch => {
            require_exact_base_head(task, repository_id)?;
        }
        Some(_) => return Err(branch_mismatch(repository_id)),
        None => {
            verify_detached_task_worktree(task, repository_id)?;
            let args = if existing_commit.is_some() {
                vec!["checkout", branch.as_str()]
            } else {
                vec![
                    "checkout",
                    "-b",
                    branch.as_str(),
                    repository.base_commit.as_str(),
                ]
            };
            run_git(
                repository_id,
                &checkout,
                &args,
                "task_repository_branch_create_failed",
                "Git could not activate the task branch",
            )?;
            require_exact_base_head(task, repository_id)?;
        }
    }
    Ok(())
}

pub(super) fn verify_task_branch(
    task: &TaskWorkspace,
    repository_id: &str,
) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let checkout = task.repository_checkout_path(repository_id);
    let branch = task.branch_name(repository_id);
    let worktree = repository.worktree.as_ref().ok_or_else(|| {
        ProjectError::new(
            "task_repository_worktree_missing",
            format!("repository '{repository_id}' task worktree is not recorded"),
        )
    })?;
    if worktree.branch.as_deref() != Some(branch.as_str()) {
        return Err(branch_mismatch(repository_id));
    }
    let entry = task_worktree_entry(&repository.source_path, &checkout)?.ok_or_else(|| {
        ProjectError::new(
            "task_repository_worktree_missing",
            format!("repository '{repository_id}' task worktree is missing"),
        )
    })?;
    if entry.is_bare
        || entry.is_prunable
        || entry.is_detached
        || entry.branch.as_deref() != Some(branch.as_str())
    {
        return Err(branch_mismatch(repository_id));
    }
    if branch_is_checked_out_elsewhere(&repository.source_path, &checkout, &branch)? {
        return Err(branch_mismatch(repository_id));
    }
    let head = git_stdout(repository_id, &checkout, &["rev-parse", "--verify", "HEAD"])?;
    let branch_head = branch_commit(repository_id, &repository.source_path, &branch)?
        .ok_or_else(|| branch_mismatch(repository_id))?;
    if head != branch_head {
        return Err(branch_mismatch(repository_id));
    }
    require_base_ancestor(repository_id, &checkout, &repository.base_commit, &head)
}

fn require_exact_base_head(task: &TaskWorkspace, repository_id: &str) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let checkout = task.repository_checkout_path(repository_id);
    let branch = task.branch_name(repository_id);
    let head = git_stdout(repository_id, &checkout, &["rev-parse", "--verify", "HEAD"])?;
    let branch_head = branch_commit(repository_id, &repository.source_path, &branch)?
        .ok_or_else(|| branch_mismatch(repository_id))?;
    if head != repository.base_commit || branch_head != repository.base_commit {
        return Err(ProjectError::new(
            "task_repository_branch_conflict",
            format!("repository '{repository_id}' task branch no longer matches its base"),
        ));
    }
    Ok(())
}

fn require_base_ancestor(
    repository_id: &str,
    checkout: &Path,
    base_commit: &str,
    head: &str,
) -> Result<(), ProjectError> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(checkout)
        .args(["merge-base", "--is-ancestor", base_commit, head])
        .output()
        .map_err(|_| branch_inspection_failed(repository_id))?;
    if output.status.success() {
        Ok(())
    } else if output.status.code() == Some(1) {
        Err(branch_mismatch(repository_id))
    } else {
        Err(branch_inspection_failed(repository_id))
    }
}

fn branch_commit(
    repository_id: &str,
    source_path: &Path,
    branch: &str,
) -> Result<Option<String>, ProjectError> {
    let exists = crate::worktree::local_branch_exists(source_path, branch)
        .map_err(|_| branch_inspection_failed(repository_id))?;
    if !exists {
        return Ok(None);
    }
    let reference = format!("refs/heads/{branch}");
    git_stdout(
        repository_id,
        source_path,
        &["rev-parse", "--verify", &reference],
    )
    .map(Some)
}

fn branch_is_checked_out_elsewhere(
    source_path: &Path,
    checkout: &Path,
    branch: &str,
) -> Result<bool, ProjectError> {
    let expected = crate::worktree::canonical_or_original(checkout);
    crate::worktree::list_existing_worktrees(source_path)
        .map_err(|_| {
            ProjectError::new(
                "task_repository_branch_inspection_failed",
                "Git could not inspect task branch ownership",
            )
        })
        .map(|entries| {
            entries.into_iter().any(|entry| {
                entry.branch.as_deref() == Some(branch)
                    && crate::worktree::canonical_or_original(&entry.path) != expected
            })
        })
}

fn run_git(
    repository_id: &str,
    cwd: &Path,
    args: &[&str],
    failure_code: &'static str,
    failure_message: &'static str,
) -> Result<(), ProjectError> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|_| {
            ProjectError::new(
                failure_code,
                format!("{failure_message} for repository '{repository_id}'"),
            )
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ProjectError::new(
            failure_code,
            format!("{failure_message} for repository '{repository_id}'"),
        ))
    }
}

fn branch_mismatch(repository_id: &str) -> ProjectError {
    ProjectError::new(
        "task_repository_branch_mismatch",
        format!("repository '{repository_id}' task branch no longer matches its ownership record"),
    )
}

fn branch_inspection_failed(repository_id: &str) -> ProjectError {
    ProjectError::new(
        "task_repository_branch_inspection_failed",
        format!("Git could not inspect repository '{repository_id}' task branch"),
    )
}
