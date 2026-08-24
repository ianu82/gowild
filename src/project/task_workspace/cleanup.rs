use std::fs;
use std::path::Path;

use super::cleanup_safety::{
    branch_commit, branch_resource, cleanup_conflict, cleanup_git_failed, cleanup_io,
    preflight_cleanup, reverse_repository_order, validate_releasable_root, worktree_resource,
};
use super::provision::{task_root_marker, task_worktree_entry, TaskWorkspaceProvisioner};
use super::runtime_layout::{
    ensure_runtime_directory_released, runtime_directories, verify_runtime_directory_released,
};
use super::{
    OwnedResource, TaskTransitionOperation, TaskTransitionState, TaskWorkspace, TaskWorkspacePhase,
};
use crate::project::ProjectError;

impl TaskWorkspaceProvisioner<'_> {
    /// Removes only resources durably owned by one stopped task. Cleanup never
    /// requires the current project manifest or execution trust, so stale and
    /// interrupted tasks remain recoverable.
    pub fn cleanup(&self, task_id: &str) -> Result<TaskWorkspace, ProjectError> {
        let _task_lock = self.states().lock_task_operations(task_id)?;
        let mut task = self.states().load(task_id)?;
        task.validate_integrity()?;
        if task.phase == TaskWorkspacePhase::Cleaned {
            return Ok(task);
        }
        if task.phase == TaskWorkspacePhase::Running {
            return Err(ProjectError::new(
                "task_workspace_running",
                "a running task must be stopped before cleanup",
            ));
        }
        if !task.runtime.ports.is_empty() {
            let broker = self.port_broker()?;
            for (name, port) in &task.runtime.ports {
                broker.verify_exact(&task.runtime.namespace, name, *port)?;
            }
        }

        preflight_cleanup(&task)?;
        if task.phase != TaskWorkspacePhase::Cleaning {
            self.transition_phase(&mut task, TaskWorkspacePhase::Cleaning)?;
        }

        for repository_id in reverse_repository_order(&task)? {
            if let Some(branch_resource) = branch_resource(&task, &repository_id) {
                if task.resource_is_owned(&branch_resource) {
                    let snapshot = task.clone();
                    let verify_id = repository_id.clone();
                    let ensure_id = repository_id.clone();
                    let _repository_lock =
                        self.states().lock_repository_operations(&repository_id)?;
                    self.ensure_released(
                        &mut task,
                        branch_resource,
                        || verify_branch_released(&snapshot, &verify_id),
                        || ensure_branch_released(&snapshot, &ensure_id),
                    )?;
                }
            }

            let worktree_resource = worktree_resource(&task, &repository_id);
            if task.resource_is_owned(&worktree_resource) {
                let snapshot = task.clone();
                let verify_id = repository_id.clone();
                let ensure_id = repository_id.clone();
                let _repository_lock = self.states().lock_repository_operations(&repository_id)?;
                self.ensure_released(
                    &mut task,
                    worktree_resource,
                    || verify_worktree_released(&snapshot, &verify_id),
                    || ensure_worktree_released(&snapshot, &ensure_id),
                )?;
            }
        }

        for (name, port) in task.runtime.ports.clone() {
            let resource = OwnedResource::PortReservation {
                name: name.clone(),
                port,
            };
            if task.resource_is_owned(&resource) {
                let snapshot = task.clone();
                self.ensure_released(
                    &mut task,
                    resource,
                    || Ok(()),
                    || self.release_port(&snapshot, &name, port),
                )?;
            }
        }

        for path in runtime_directories(&task).into_iter().rev() {
            let resource = OwnedResource::RuntimeDirectory { path: path.clone() };
            if task.resource_is_owned(&resource) {
                let verify_path = path.clone();
                let snapshot = task.clone();
                self.ensure_released(
                    &mut task,
                    resource,
                    || verify_runtime_directory_released(&verify_path),
                    || ensure_runtime_directory_released(&snapshot, &path),
                )?;
            }
        }

        let root_resource = OwnedResource::WorkspaceDirectory {
            path: task.root.clone(),
        };
        if task.resource_is_owned(&root_resource) {
            let snapshot = task.clone();
            self.ensure_released(
                &mut task,
                root_resource,
                || verify_task_root_released(&snapshot),
                || ensure_task_root_released(&snapshot),
            )?;
        }

        self.transition_phase(&mut task, TaskWorkspacePhase::Cleaned)?;
        Ok(task)
    }

    pub(super) fn ensure_released(
        &self,
        task: &mut TaskWorkspace,
        resource: OwnedResource,
        verify: impl FnOnce() -> Result<(), ProjectError>,
        ensure: impl FnOnce() -> Result<(), ProjectError>,
    ) -> Result<(), ProjectError> {
        if !task.resource_is_owned(&resource) {
            return verify();
        }
        let sequence = match task.journal.iter().find(|transition| {
            transition.operation == TaskTransitionOperation::Release
                && transition.state == TaskTransitionState::Planned
                && transition.resource == resource
        }) {
            Some(transition) => transition.sequence,
            None => {
                let expected_revision = task.revision;
                let sequence = task.plan_transition(TaskTransitionOperation::Release, resource)?;
                self.states().save(task, expected_revision)?;
                sequence
            }
        };

        if let Err(error) = ensure() {
            self.record_failed_transition(task, sequence, error.code)?;
            return Err(error);
        }
        let expected_revision = task.revision;
        task.finish_transition(sequence, TaskTransitionState::Applied, None)?;
        self.states().save(task, expected_revision)
    }
}

pub(super) fn ensure_branch_released(
    task: &TaskWorkspace,
    repository_id: &str,
) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let checkout = task.repository_checkout_path(repository_id);
    let branch = task.branch_name(repository_id);
    let reference = format!("refs/heads/{branch}");
    let existing = branch_commit(repository_id, &repository.source_path, &branch)?;
    if let Some(entry) = task_worktree_entry(&repository.source_path, &checkout)? {
        match entry.branch.as_deref() {
            Some(current) if current == branch => {
                let head = existing.as_deref().ok_or_else(|| {
                    cleanup_conflict(format!(
                        "repository '{repository_id}' task branch is missing"
                    ))
                })?;
                run_git(repository_id, &checkout, &["checkout", "--detach", head])?;
            }
            Some(_) => {
                return Err(cleanup_conflict(format!(
                    "repository '{repository_id}' task worktree is on another branch"
                )));
            }
            None if entry.is_detached => {}
            None => {
                return Err(cleanup_conflict(format!(
                    "repository '{repository_id}' task worktree is not detached"
                )));
            }
        }
    }
    if let Some(commit) = existing {
        run_git(
            repository_id,
            &repository.source_path,
            &["update-ref", "-d", &reference, &commit],
        )?;
    }
    verify_branch_released(task, repository_id)
}

fn verify_branch_released(task: &TaskWorkspace, repository_id: &str) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let checkout = task.repository_checkout_path(repository_id);
    let branch = task.branch_name(repository_id);
    if branch_commit(repository_id, &repository.source_path, &branch)?.is_some() {
        return Err(cleanup_conflict(format!(
            "repository '{repository_id}' task branch still exists"
        )));
    }
    if task_worktree_entry(&repository.source_path, &checkout)?
        .and_then(|entry| entry.branch)
        .as_deref()
        == Some(branch.as_str())
    {
        return Err(cleanup_conflict(format!(
            "repository '{repository_id}' task branch is still checked out"
        )));
    }
    Ok(())
}

pub(super) fn ensure_worktree_released(
    task: &TaskWorkspace,
    repository_id: &str,
) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let checkout = task.repository_checkout_path(repository_id);
    match task_worktree_entry(&repository.source_path, &checkout)? {
        Some(_) => {
            let command = crate::worktree::build_worktree_remove_command(
                &repository.source_path,
                &checkout,
                false,
            );
            crate::worktree::run_worktree_command(&command).map_err(|_| {
                cleanup_git_failed(format!(
                    "Git refused to remove repository '{repository_id}' task worktree"
                ))
            })?;
        }
        None if !checkout.exists() => {}
        None => {
            return Err(cleanup_conflict(format!(
                "repository '{repository_id}' checkout contains unowned data"
            )));
        }
    }
    verify_worktree_released(task, repository_id)
}

fn verify_worktree_released(task: &TaskWorkspace, repository_id: &str) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let checkout = task.repository_checkout_path(repository_id);
    if task_worktree_entry(&repository.source_path, &checkout)?.is_some() || checkout.exists() {
        Err(cleanup_conflict(format!(
            "repository '{repository_id}' task worktree still exists"
        )))
    } else {
        Ok(())
    }
}

pub(super) fn ensure_task_root_released(task: &TaskWorkspace) -> Result<(), ProjectError> {
    if !task.root.exists() {
        return Ok(());
    }
    validate_releasable_root(task, true)?;
    let repositories = task.root.join("repositories");
    if repositories.exists() {
        fs::remove_dir(&repositories)
            .map_err(|error| cleanup_io("repository directory", &error))?;
    }
    let marker = task_root_marker(task);
    if marker.exists() {
        fs::remove_dir(&marker).map_err(|error| cleanup_io("ownership marker", &error))?;
    }
    fs::remove_dir(&task.root).map_err(|error| cleanup_io("task workspace root", &error))?;
    verify_task_root_released(task)
}

fn verify_task_root_released(task: &TaskWorkspace) -> Result<(), ProjectError> {
    if task.root.exists() {
        Err(cleanup_conflict(
            "task workspace root still contains data".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn run_git(repository_id: &str, cwd: &Path, args: &[&str]) -> Result<(), ProjectError> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|_| {
            cleanup_git_failed(format!(
                "Git could not release repository '{repository_id}' task resources"
            ))
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(cleanup_git_failed(format!(
            "Git could not release repository '{repository_id}' task resources"
        )))
    }
}
