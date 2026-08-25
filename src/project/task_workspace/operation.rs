use super::TaskWorkspace;
use crate::project::ProjectError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOperationProgress {
    pub task_id: String,
    pub stage: TaskOperationStage,
    pub completed_steps: usize,
    pub total_steps: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOperationStage {
    Validating,
    WorkspaceRoot,
    RuntimeLayout,
    Repository { repository_id: String },
    RepositoryBranch { repository_id: String },
    RepositoryWorktree { repository_id: String },
    Port { name: String },
    RuntimeDirectory { path: std::path::PathBuf },
    Finalizing,
    Complete,
}

/// Cooperative control observed only between durable lifecycle boundaries.
/// Implementations must keep callbacks fast and must not mutate task state.
pub trait TaskOperationControl: Send + Sync {
    fn is_cancelled(&self) -> bool;
    fn report_progress(&self, progress: &TaskOperationProgress);
}

pub(super) struct UncontrolledTaskOperation;

impl TaskOperationControl for UncontrolledTaskOperation {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn report_progress(&self, _progress: &TaskOperationProgress) {}
}

pub(super) fn report_progress(
    control: &(impl TaskOperationControl + ?Sized),
    task_id: &str,
    stage: TaskOperationStage,
    completed_steps: usize,
    total_steps: usize,
) -> Result<(), ProjectError> {
    let progress = TaskOperationProgress {
        task_id: task_id.to_string(),
        stage,
        completed_steps,
        total_steps,
    };
    control.report_progress(&progress);
    require_active(control)
}

pub(super) fn require_active(
    control: &(impl TaskOperationControl + ?Sized),
) -> Result<(), ProjectError> {
    if control.is_cancelled() {
        Err(ProjectError::new(
            "project_task_operation_cancelled",
            "project task operation was cancelled at a durable boundary",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn complete_progress(
    control: &(impl TaskOperationControl + ?Sized),
    task_id: &str,
    total_steps: usize,
) {
    control.report_progress(&TaskOperationProgress {
        task_id: task_id.to_string(),
        stage: TaskOperationStage::Complete,
        completed_steps: total_steps,
        total_steps,
    });
}

pub(super) fn provision_step_count(task: &TaskWorkspace) -> usize {
    task.repositories.len().saturating_add(3)
}

pub(super) fn cleanup_step_count(task: &TaskWorkspace) -> usize {
    let repository_steps = task
        .repositories
        .keys()
        .map(|repository_id| {
            usize::from(
                super::cleanup_safety::branch_resource(task, repository_id)
                    .is_some_and(|resource| task.resource_is_owned(&resource)),
            ) + usize::from(
                task.resource_is_owned(&super::cleanup_safety::worktree_resource(
                    task,
                    repository_id,
                )),
            )
        })
        .sum::<usize>();
    let port_steps = task
        .runtime
        .ports
        .iter()
        .filter(|(name, port)| {
            task.resource_is_owned(&super::OwnedResource::PortReservation {
                name: (*name).clone(),
                port: **port,
            })
        })
        .count();
    let runtime_steps = super::runtime_layout::runtime_directories(task)
        .into_iter()
        .filter(|path| {
            task.resource_is_owned(&super::OwnedResource::RuntimeDirectory { path: path.clone() })
        })
        .count();
    let root_step = usize::from(task.resource_is_owned(
        &super::OwnedResource::WorkspaceDirectory {
            path: task.root.clone(),
        },
    ));
    repository_steps
        .saturating_add(port_steps)
        .saturating_add(runtime_steps)
        .saturating_add(root_step)
        .saturating_add(1)
}
