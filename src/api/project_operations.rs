use crate::api::schema::{
    ErrorBody, ErrorResponse, ProjectTaskLifecycleParams, ProjectTaskOperationError,
    ProjectTaskOperationInfo, ProjectTaskOperationKind, ProjectTaskOperationParams,
    ProjectTaskOperationProgress, ProjectTaskOperationStage, ProjectTaskOperationStatus,
    ResponseResult, SuccessResponse, PROJECT_TASK_API_VERSION,
};
use crate::project::task_operations::{
    ProjectTaskOperationKind as DomainOperationKind, ProjectTaskOperationRegistry,
    ProjectTaskOperationSnapshot, ProjectTaskOperationStatus as DomainOperationStatus,
};
use crate::project::task_workspace::TaskOperationStage;
use crate::project::{ProjectError, ProjectTaskReader};

pub(super) fn task_provision(
    id: String,
    params: ProjectTaskLifecycleParams,
    operations: &ProjectTaskOperationRegistry,
) -> String {
    start_lifecycle_operation(id, params, operations, DomainOperationKind::Provision)
}

pub(super) fn task_cleanup(
    id: String,
    params: ProjectTaskLifecycleParams,
    operations: &ProjectTaskOperationRegistry,
) -> String {
    start_lifecycle_operation(id, params, operations, DomainOperationKind::Cleanup)
}

pub(super) fn operation_get(
    id: String,
    params: ProjectTaskOperationParams,
    operations: &ProjectTaskOperationRegistry,
) -> String {
    encode_operation(id, operations.get(&params.operation_id))
}

pub(super) fn operation_cancel(
    id: String,
    params: ProjectTaskOperationParams,
    operations: &ProjectTaskOperationRegistry,
) -> String {
    encode_operation(id, operations.cancel(&params.operation_id))
}

fn start_lifecycle_operation(
    id: String,
    params: ProjectTaskLifecycleParams,
    operations: &ProjectTaskOperationRegistry,
    kind: DomainOperationKind,
) -> String {
    if let Err(error) = ProjectTaskReader::validate_task_id(&params.task_id) {
        return encode_error(id, error);
    }
    let path = match super::projects::project_path(&params.path) {
        Ok(path) => path,
        Err(error) => return encode_error(id, error),
    };
    let result = match kind {
        DomainOperationKind::Provision => operations.start_provision(path, &params.task_id),
        DomainOperationKind::Cleanup => operations.start_cleanup(path, &params.task_id),
    };
    encode_operation(id, result)
}

fn encode_operation(
    id: String,
    result: Result<ProjectTaskOperationSnapshot, ProjectError>,
) -> String {
    match result {
        Ok(operation) => encode_success(
            id,
            ResponseResult::ProjectTaskOperation {
                schema_version: PROJECT_TASK_API_VERSION,
                operation: operation_info(operation),
            },
        ),
        Err(error) => encode_error(id, error),
    }
}

pub(super) fn operation_event(
    operation: ProjectTaskOperationSnapshot,
) -> crate::api::schema::EventEnvelope {
    crate::api::schema::EventEnvelope {
        event: crate::api::schema::EventKind::ProjectTaskOperationChanged,
        data: crate::api::schema::EventData::ProjectTaskOperationChanged {
            operation: operation_info(operation),
        },
    }
}

fn operation_info(operation: ProjectTaskOperationSnapshot) -> ProjectTaskOperationInfo {
    ProjectTaskOperationInfo {
        operation_id: operation.operation_id,
        project_id: operation.project_id,
        project_root: operation.project_root.to_string_lossy().into_owned(),
        task_id: operation.task_id,
        kind: match operation.kind {
            DomainOperationKind::Provision => ProjectTaskOperationKind::Provision,
            DomainOperationKind::Cleanup => ProjectTaskOperationKind::Cleanup,
        },
        status: match operation.status {
            DomainOperationStatus::Queued => ProjectTaskOperationStatus::Queued,
            DomainOperationStatus::Running => ProjectTaskOperationStatus::Running,
            DomainOperationStatus::Succeeded => ProjectTaskOperationStatus::Succeeded,
            DomainOperationStatus::Failed => ProjectTaskOperationStatus::Failed,
            DomainOperationStatus::Cancelled => ProjectTaskOperationStatus::Cancelled,
        },
        cancellation_requested: operation.cancellation_requested,
        progress: operation
            .progress
            .map(|progress| ProjectTaskOperationProgress {
                stage: operation_stage(progress.stage),
                completed_steps: progress.completed_steps,
                total_steps: progress.total_steps,
            }),
        error: operation.error.map(|error| ProjectTaskOperationError {
            code: error.code.to_string(),
            message: error.message,
        }),
    }
}

fn operation_stage(stage: TaskOperationStage) -> ProjectTaskOperationStage {
    match stage {
        TaskOperationStage::Validating => ProjectTaskOperationStage::Validating,
        TaskOperationStage::WorkspaceRoot => ProjectTaskOperationStage::WorkspaceRoot,
        TaskOperationStage::RuntimeLayout => ProjectTaskOperationStage::RuntimeLayout,
        TaskOperationStage::Repository { repository_id } => {
            ProjectTaskOperationStage::Repository { repository_id }
        }
        TaskOperationStage::RepositoryBranch { repository_id } => {
            ProjectTaskOperationStage::RepositoryBranch { repository_id }
        }
        TaskOperationStage::RepositoryWorktree { repository_id } => {
            ProjectTaskOperationStage::RepositoryWorktree { repository_id }
        }
        TaskOperationStage::Port { name } => ProjectTaskOperationStage::Port { name },
        TaskOperationStage::RuntimeDirectory { path } => {
            ProjectTaskOperationStage::RuntimeDirectory {
                path: path.to_string_lossy().into_owned(),
            }
        }
        TaskOperationStage::Finalizing => ProjectTaskOperationStage::Finalizing,
        TaskOperationStage::Complete => ProjectTaskOperationStage::Complete,
    }
}

fn encode_success(id: String, result: ResponseResult) -> String {
    serde_json::to_string(&SuccessResponse { id, result }).unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"internal_error","message":"failed to encode response"}}"#
            .to_string()
    })
}

fn encode_error(id: String, error: ProjectError) -> String {
    serde_json::to_string(&ErrorResponse {
        id,
        error: ErrorBody {
            code: error.code.to_string(),
            message: error.message,
        },
    })
    .unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"internal_error","message":"failed to encode response"}}"#
            .to_string()
    })
}

#[cfg(test)]
mod tests;
