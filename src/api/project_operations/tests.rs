use std::path::Path;
use std::time::{Duration, Instant};

use super::*;
use crate::project::task_workspace::provision_tests::ProjectFixture;
use crate::project::task_workspace::{TaskAgent, TaskProtocol, TaskRoute};
use crate::project::{CreateProjectTask, ProjectTaskService};

struct StateHomeGuard(Option<std::ffi::OsString>);

impl Drop for StateHomeGuard {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            std::env::set_var("XDG_STATE_HOME", value);
        } else {
            std::env::remove_var("XDG_STATE_HOME");
        }
    }
}

#[test]
fn lifecycle_handlers_start_and_observe_real_workers_without_app_dispatch() {
    let _lock = crate::config::test_config_env_lock().lock().unwrap();
    let _state_home_guard = StateHomeGuard(std::env::var_os("XDG_STATE_HOME"));
    let fixture = ProjectFixture::new(false);
    std::env::set_var(
        "XDG_STATE_HOME",
        fixture.root.join("operation-api-state-home"),
    );
    std::fs::write(
        &fixture.definition.manifest_path,
        crate::project::render_manifest(&fixture.definition.manifest).unwrap(),
    )
    .unwrap();
    ProjectTaskService::open(&fixture.root)
        .unwrap()
        .create(CreateProjectTask {
            task_id: "api-lifecycle".into(),
            outcome: "Coordinate all three repositories".into(),
            agent: TaskAgent::Codex,
            route: TaskRoute {
                gateway_id: "mindshub".into(),
                protocol: TaskProtocol::OpenAiResponses,
                model: "provider/team/model".into(),
            },
        })
        .unwrap();
    let operations = ProjectTaskOperationRegistry::default();
    let params = ProjectTaskLifecycleParams {
        path: fixture.root.to_string_lossy().into_owned(),
        task_id: "api-lifecycle".into(),
    };

    let started = task_provision("provision".into(), params.clone(), &operations);
    let started: SuccessResponse = serde_json::from_str(&started).unwrap();
    let operation_id = match started.result {
        ResponseResult::ProjectTaskOperation { operation, .. } => operation.operation_id,
        other => panic!("unexpected response: {other:?}"),
    };
    let provisioned = wait_for_terminal(&operations, &operation_id);
    assert_eq!(provisioned.status, ProjectTaskOperationStatus::Succeeded);
    assert_eq!(
        provisioned.project_root,
        fixture.root.to_string_lossy().into_owned()
    );
    let terminal_cancel = operation_cancel(
        "cancel-complete".into(),
        ProjectTaskOperationParams {
            operation_id: operation_id.clone(),
        },
        &operations,
    );
    let terminal_cancel: SuccessResponse = serde_json::from_str(&terminal_cancel).unwrap();
    let terminal_cancel = match terminal_cancel.result {
        ResponseResult::ProjectTaskOperation { operation, .. } => operation,
        other => panic!("unexpected response: {other:?}"),
    };
    assert_eq!(
        terminal_cancel.status,
        ProjectTaskOperationStatus::Succeeded
    );
    assert!(!terminal_cancel.cancellation_requested);

    let cleanup = task_cleanup("cleanup".into(), params, &operations);
    let cleanup: SuccessResponse = serde_json::from_str(&cleanup).unwrap();
    let operation_id = match cleanup.result {
        ResponseResult::ProjectTaskOperation { operation, .. } => operation.operation_id,
        other => panic!("unexpected response: {other:?}"),
    };
    assert_eq!(
        wait_for_terminal(&operations, &operation_id).status,
        ProjectTaskOperationStatus::Succeeded
    );
}

#[test]
fn lifecycle_inputs_are_rejected_before_filesystem_access() {
    let operations = ProjectTaskOperationRegistry::default();
    let invalid_task = task_provision(
        "invalid-task".into(),
        ProjectTaskLifecycleParams {
            path: "/path/that/does/not/exist".into(),
            task_id: "../escape".into(),
        },
        &operations,
    );
    let invalid_task: ErrorResponse = serde_json::from_str(&invalid_task).unwrap();
    assert_eq!(invalid_task.error.code, "invalid_project_task_id");

    let invalid_path = task_cleanup(
        "invalid-path".into(),
        ProjectTaskLifecycleParams {
            path: "bad\0path".into(),
            task_id: "safe-task".into(),
        },
        &operations,
    );
    let invalid_path: ErrorResponse = serde_json::from_str(&invalid_path).unwrap();
    assert_eq!(invalid_path.error.code, "invalid_project_path");

    let invalid_operation = operation_get(
        "invalid-operation".into(),
        ProjectTaskOperationParams {
            operation_id: "../escape".into(),
        },
        &operations,
    );
    let invalid_operation: ErrorResponse = serde_json::from_str(&invalid_operation).unwrap();
    assert_eq!(
        invalid_operation.error.code,
        "invalid_project_task_operation_id"
    );
    assert!(!Path::new("/path/that/does/not/exist").exists());
}

fn wait_for_terminal(
    operations: &ProjectTaskOperationRegistry,
    operation_id: &str,
) -> ProjectTaskOperationInfo {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = operation_get(
            "get".into(),
            ProjectTaskOperationParams {
                operation_id: operation_id.into(),
            },
            operations,
        );
        let response: SuccessResponse = serde_json::from_str(&response).unwrap();
        let operation = match response.result {
            ResponseResult::ProjectTaskOperation { operation, .. } => operation,
            other => panic!("unexpected response: {other:?}"),
        };
        if matches!(
            operation.status,
            ProjectTaskOperationStatus::Succeeded
                | ProjectTaskOperationStatus::Failed
                | ProjectTaskOperationStatus::Cancelled
        ) {
            return operation;
        }
        assert!(Instant::now() < deadline, "operation did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}
