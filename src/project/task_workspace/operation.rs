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

pub(super) fn provision_step_count(task: &TaskWorkspace) -> usize {
    task.repositories.len().saturating_add(3)
}
