use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::repository::{
    directory_is_empty, ensure_private_directory_chain, restrict_directory_permissions,
    validate_existing_ancestors, TaskWorkspaceRepository,
};
use super::{
    LoadedProject, OwnedResource, TaskTransitionOperation, TaskTransitionState, TaskWorkspace,
    TaskWorkspacePhase,
};
use crate::project::{ProjectDefinition, ProjectError, ProjectPrivateState};

const TASK_ROOT_MARKER_PREFIX: &str = ".gowild-task-workspace-v1";

/// Executes crash-recoverable task provisioning while an OS-backed per-task
/// operation lease prevents a second process from interleaving mutations.
pub struct TaskWorkspaceProvisioner<'a> {
    states: &'a TaskWorkspaceRepository,
    ports: Option<&'a super::TaskPortBroker>,
}

impl<'a> TaskWorkspaceProvisioner<'a> {
    pub fn new(states: &'a TaskWorkspaceRepository) -> Self {
        Self {
            states,
            ports: None,
        }
    }

    pub fn with_port_broker(
        states: &'a TaskWorkspaceRepository,
        ports: &'a super::TaskPortBroker,
    ) -> Self {
        Self {
            states,
            ports: Some(ports),
        }
    }

    pub(super) fn states(&self) -> &TaskWorkspaceRepository {
        self.states
    }

    pub(super) fn ports(&self) -> Option<&super::TaskPortBroker> {
        self.ports
    }

    pub fn provision(
        &self,
        definition: &ProjectDefinition,
        private_state: &ProjectPrivateState,
        project: &LoadedProject,
        task_id: &str,
    ) -> Result<TaskWorkspace, ProjectError> {
        require_matching_definition(definition, project)?;
        private_state.require_execution_trust(definition)?;
        let _operation_lock = self.states.lock_task_operations(task_id)?;
        let mut task = self.states.load(task_id)?;
        task.validate(project)?;

        match task.phase {
            TaskWorkspacePhase::Planned | TaskWorkspacePhase::NeedsAttention => {
                self.transition_phase(&mut task, TaskWorkspacePhase::Provisioning)?;
            }
            TaskWorkspacePhase::Provisioning => {}
            TaskWorkspacePhase::Ready
            | TaskWorkspacePhase::Running
            | TaskWorkspacePhase::Stopped => {
                verify_provisioned_task(&task)?;
                return Ok(task);
            }
            TaskWorkspacePhase::Cleaning | TaskWorkspacePhase::Cleaned => {
                return Err(ProjectError::new(
                    "task_workspace_not_provisionable",
                    "a cleaning or cleaned task workspace cannot be provisioned",
                ));
            }
        }

        let root_resource = OwnedResource::WorkspaceDirectory {
            path: task.root.clone(),
        };
        let root_snapshot = task.clone();
        self.ensure_acquired(
            &mut task,
            root_resource,
            || verify_owned_task_root(&root_snapshot),
            || ensure_owned_task_root(&root_snapshot),
        )?;
        self.ensure_runtime_layout(&mut task)?;

        for repository_id in project.manifest.dependency_order()? {
            if task.repositories[&repository_id].worktree.is_some() {
                verify_task_repository(&task, &repository_id)?;
                continue;
            }
            let repository = &task.repositories[&repository_id];
            let resource = OwnedResource::RepositoryWorktree {
                repository_id: repository_id.clone(),
                source_path: repository.source_path.clone(),
                checkout_path: task.repository_checkout_path(&repository_id),
                base_commit: repository.base_commit.clone(),
            };
            let worktree_snapshot = task.clone();
            let verify_repository_id = repository_id.clone();
            let ensure_repository_id = repository_id.clone();
            self.ensure_acquired(
                &mut task,
                resource,
                || {
                    let _repository_lock = self
                        .states
                        .lock_repository_operations(&verify_repository_id)?;
                    verify_detached_task_worktree(&worktree_snapshot, &verify_repository_id)
                },
                || {
                    let _repository_lock = self
                        .states
                        .lock_repository_operations(&ensure_repository_id)?;
                    ensure_detached_task_worktree(&worktree_snapshot, &ensure_repository_id)
                },
            )?;
        }

        self.transition_phase(&mut task, TaskWorkspacePhase::Ready)?;
        verify_provisioned_task(&task)?;
        Ok(task)
    }

    pub(super) fn transition_phase(
        &self,
        task: &mut TaskWorkspace,
        phase: TaskWorkspacePhase,
    ) -> Result<(), ProjectError> {
        let expected_revision = task.revision;
        task.transition_phase(phase)?;
        self.states.save(task, expected_revision)
    }

    pub(super) fn ensure_acquired(
        &self,
        task: &mut TaskWorkspace,
        resource: OwnedResource,
        verify: impl FnOnce() -> Result<(), ProjectError>,
        ensure: impl FnOnce() -> Result<(), ProjectError>,
    ) -> Result<(), ProjectError> {
        if task.resource_is_owned(&resource) {
            return verify();
        }
        let sequence = match task.journal.iter().find(|transition| {
            transition.operation == TaskTransitionOperation::Acquire
                && transition.state == TaskTransitionState::Planned
                && transition.resource == resource
        }) {
            Some(transition) => transition.sequence,
            None => {
                let expected_revision = task.revision;
                let sequence = task.plan_transition(TaskTransitionOperation::Acquire, resource)?;
                self.states.save(task, expected_revision)?;
                sequence
            }
        };

        if let Err(error) = ensure() {
            self.record_failed_transition(task, sequence, error.code)?;
            return Err(error);
        }
        let expected_revision = task.revision;
        task.finish_transition(sequence, TaskTransitionState::Applied, None)?;
        self.states.save(task, expected_revision)
    }

    pub(super) fn record_failed_transition(
        &self,
        task: &mut TaskWorkspace,
        sequence: u64,
        failure_code: &'static str,
    ) -> Result<(), ProjectError> {
        let expected_revision = task.revision;
        task.finish_transition(sequence, TaskTransitionState::Failed, Some(failure_code))?;
        self.states.save(task, expected_revision)?;
        if task.phase != TaskWorkspacePhase::NeedsAttention {
            let expected_revision = task.revision;
            task.transition_phase(TaskWorkspacePhase::NeedsAttention)?;
            self.states.save(task, expected_revision)?;
        }
        Ok(())
    }
}

fn verify_provisioned_task(task: &TaskWorkspace) -> Result<(), ProjectError> {
    let root_resource = OwnedResource::WorkspaceDirectory {
        path: task.root.clone(),
    };
    if !task.resource_is_owned(&root_resource) {
        return Err(ProjectError::new(
            "task_workspace_ownership_mismatch",
            "task workspace journal does not own its data-plane root",
        ));
    }
    verify_owned_task_root(task)?;
    super::runtime_layout::verify_runtime_layout(task)?;
    for repository_id in task.repositories.keys() {
        if task.repositories[repository_id].worktree.is_none() {
            return Err(ProjectError::new(
                "task_repository_worktree_missing",
                format!("repository '{repository_id}' task worktree is not recorded"),
            ));
        }
        verify_task_repository(task, repository_id)?;
    }
    Ok(())
}

fn verify_task_repository(task: &TaskWorkspace, repository_id: &str) -> Result<(), ProjectError> {
    if task.repositories[repository_id]
        .worktree
        .as_ref()
        .and_then(|worktree| worktree.branch.as_ref())
        .is_some()
    {
        super::branch::verify_task_branch(task, repository_id)
    } else {
        verify_detached_task_worktree(task, repository_id)
    }
}

pub(super) fn task_root_marker(task: &TaskWorkspace) -> PathBuf {
    task.root.join(format!(
        "{TASK_ROOT_MARKER_PREFIX}-{}-{}-{}",
        task.project_id, task.id, task.manifest_digest
    ))
}

pub(super) fn ensure_owned_task_root(task: &TaskWorkspace) -> Result<(), ProjectError> {
    validate_existing_ancestors(&task.root)?;
    let created = ensure_private_directory_chain(&task.root)?;
    let marker = task_root_marker(task);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(ProjectError::new(
                "task_workspace_root_ownership_mismatch",
                "task workspace ownership marker is not a private directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !created && !directory_is_empty(&task.root)? {
                return Err(ProjectError::new(
                    "task_workspace_root_ownership_mismatch",
                    "refusing to adopt a non-empty task workspace without its ownership marker",
                ));
            }
            crate::platform::create_private_directory(&marker)
                .map_err(|error| task_root_io_error("marker creation", &error))?;
        }
        Err(error) => return Err(task_root_io_error("marker metadata", &error)),
    }
    restrict_directory_permissions(&task.root)?;
    restrict_directory_permissions(&marker)?;
    ensure_private_directory_chain(&task.root.join("repositories"))?;
    Ok(())
}

pub(super) fn verify_owned_task_root(task: &TaskWorkspace) -> Result<(), ProjectError> {
    validate_existing_ancestors(&task.root)?;
    let root = fs::symlink_metadata(&task.root).map_err(|_| {
        ProjectError::new(
            "task_workspace_root_missing",
            "task workspace root is missing",
        )
    })?;
    let marker = fs::symlink_metadata(task_root_marker(task)).map_err(|_| {
        ProjectError::new(
            "task_workspace_root_ownership_mismatch",
            "task workspace ownership marker is missing",
        )
    })?;
    let repositories = fs::symlink_metadata(task.root.join("repositories")).map_err(|_| {
        ProjectError::new(
            "task_workspace_root_missing",
            "task workspace repository directory is missing",
        )
    })?;
    if root.file_type().is_symlink()
        || !root.is_dir()
        || marker.file_type().is_symlink()
        || !marker.is_dir()
        || repositories.file_type().is_symlink()
        || !repositories.is_dir()
    {
        return Err(ProjectError::new(
            "task_workspace_root_ownership_mismatch",
            "task workspace root, marker, or repository directory is not owned",
        ));
    }
    Ok(())
}

pub(super) fn ensure_detached_task_worktree(
    task: &TaskWorkspace,
    repository_id: &str,
) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let checkout = task.repository_checkout_path(repository_id);
    match task_worktree_entry(&repository.source_path, &checkout)? {
        Some(_) => verify_detached_task_worktree(task, repository_id),
        None => {
            match fs::symlink_metadata(&checkout) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(task_root_io_error("checkout metadata", &error)),
                Ok(_) => {
                    return Err(ProjectError::new(
                        "task_repository_worktree_conflict",
                        format!(
                            "repository '{repository_id}' checkout path already contains unowned data"
                        ),
                    ));
                }
            }
            let command = crate::worktree::build_worktree_add_detached_command(
                &repository.source_path,
                &checkout,
                &repository.base_commit,
            );
            crate::worktree::run_worktree_command(&command).map_err(|_| {
                ProjectError::new(
                    "task_repository_worktree_create_failed",
                    format!("Git could not create the '{repository_id}' task worktree"),
                )
            })?;
            verify_detached_task_worktree(task, repository_id)
        }
    }
}

pub(super) fn verify_detached_task_worktree(
    task: &TaskWorkspace,
    repository_id: &str,
) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let checkout = task.repository_checkout_path(repository_id);
    let entry = task_worktree_entry(&repository.source_path, &checkout)?.ok_or_else(|| {
        ProjectError::new(
            "task_repository_worktree_missing",
            format!("repository '{repository_id}' task worktree is missing"),
        )
    })?;
    let head = git_stdout(repository_id, &checkout, &["rev-parse", "--verify", "HEAD"])?;
    if entry.is_bare
        || entry.is_prunable
        || !entry.is_detached
        || entry.branch.is_some()
        || head != repository.base_commit
    {
        return Err(ProjectError::new(
            "task_repository_worktree_mismatch",
            format!("repository '{repository_id}' task worktree no longer matches its base"),
        ));
    }
    Ok(())
}

pub(super) fn task_worktree_entry(
    source_path: &Path,
    checkout_path: &Path,
) -> Result<Option<crate::worktree::ExistingWorktree>, ProjectError> {
    if let Ok(metadata) = fs::symlink_metadata(checkout_path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ProjectError::new(
                "task_repository_worktree_mismatch",
                "task worktree checkout is a symlink or non-directory",
            ));
        }
    }
    let expected = crate::worktree::canonical_or_original(checkout_path);
    let entries = crate::worktree::list_existing_worktrees(source_path).map_err(|_| {
        ProjectError::new(
            "task_repository_worktree_inspection_failed",
            "Git could not inspect task worktrees",
        )
    })?;
    let mut matches = entries
        .into_iter()
        .filter(|entry| crate::worktree::canonical_or_original(&entry.path) == expected);
    let result = matches.next();
    if matches.next().is_some() {
        return Err(ProjectError::new(
            "task_repository_worktree_mismatch",
            "Git reported duplicate task worktrees at the owned checkout path",
        ));
    }
    Ok(result)
}

pub(super) fn git_stdout(
    repository_id: &str,
    cwd: &Path,
    args: &[&str],
) -> Result<String, ProjectError> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|_| {
            ProjectError::new(
                "task_repository_git_unavailable",
                format!("Git became unavailable for repository '{repository_id}'"),
            )
        })?;
    let value = String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if output.status.success() {
        value.ok_or_else(|| {
            ProjectError::new(
                "task_repository_git_invalid_output",
                format!("Git returned no result for repository '{repository_id}'"),
            )
        })
    } else {
        Err(ProjectError::new(
            "task_repository_git_command_failed",
            format!("Git could not inspect repository '{repository_id}'"),
        ))
    }
}

pub(super) fn require_matching_definition(
    definition: &ProjectDefinition,
    project: &LoadedProject,
) -> Result<(), ProjectError> {
    if definition.manifest_path != project.manifest_path
        || definition.root != project.root
        || definition.digest != project.digest
        || definition.manifest.id != project.manifest.id
    {
        return Err(ProjectError::new(
            "project_definition_mismatch",
            "resolved project does not belong to the supplied manifest definition",
        ));
    }
    Ok(())
}

fn task_root_io_error(operation: &'static str, error: &io::Error) -> ProjectError {
    ProjectError::new(
        "task_workspace_root_io",
        format!(
            "task workspace root {operation} failed ({:?})",
            error.kind()
        ),
    )
}
