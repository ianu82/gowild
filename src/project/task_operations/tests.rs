use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use crate::project::task_workspace::provision_tests::ProjectFixture;
use crate::project::task_workspace::{
    TaskAgent, TaskOperationStage, TaskProtocol, TaskRoute, TaskWorkspacePhase,
};
use crate::project::{CreateProjectTask, ProjectTaskReader};

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
fn registry_runs_real_service_lifecycle_workers() {
    let _lock = crate::config::test_config_env_lock().lock().unwrap();
    let previous_state_home = std::env::var_os("XDG_STATE_HOME");
    let _state_home_guard = StateHomeGuard(previous_state_home);
    let fixture = ProjectFixture::new(false);
    std::env::set_var("XDG_STATE_HOME", fixture.root.join("operation-state-home"));
    std::fs::write(
        &fixture.definition.manifest_path,
        crate::project::render_manifest(&fixture.definition.manifest).unwrap(),
    )
    .unwrap();
    ProjectTaskService::open(&fixture.root)
        .unwrap()
        .create(CreateProjectTask {
            task_id: "registry-lifecycle".into(),
            outcome: "Coordinate the three repositories".into(),
            agent: TaskAgent::Codex,
            route: TaskRoute {
                gateway_id: "mindshub".into(),
                protocol: TaskProtocol::OpenAiResponses,
                model: "provider/team/model".into(),
            },
        })
        .unwrap();
    let registry = ProjectTaskOperationRegistry::default();

    let provision = registry
        .start_provision(&fixture.root, "registry-lifecycle")
        .unwrap();
    assert_eq!(
        wait_until_terminal(&registry, &provision.operation_id).status,
        ProjectTaskOperationStatus::Succeeded
    );
    assert_eq!(
        ProjectTaskReader::open(&fixture.root)
            .unwrap()
            .get("registry-lifecycle")
            .unwrap()
            .task
            .phase,
        TaskWorkspacePhase::Ready
    );

    let cleanup = registry
        .start_cleanup(&fixture.root, "registry-lifecycle")
        .unwrap();
    assert_eq!(
        wait_until_terminal(&registry, &cleanup.operation_id).status,
        ProjectTaskOperationStatus::Succeeded
    );
    assert_eq!(
        ProjectTaskReader::open(&fixture.root)
            .unwrap()
            .get("registry-lifecycle")
            .unwrap()
            .task
            .phase,
        TaskWorkspacePhase::Cleaned
    );
}

#[test]
fn registry_reports_progress_and_terminal_success() {
    let registry = ProjectTaskOperationRegistry::default();
    let started = registry
        .start_operation(
            "project".into(),
            PathBuf::from("/project"),
            "task".into(),
            ProjectTaskOperationKind::Provision,
            |control| {
                control.report_progress(&TaskOperationProgress {
                    task_id: "task".into(),
                    stage: TaskOperationStage::Finalizing,
                    completed_steps: 3,
                    total_steps: 4,
                });
                control.report_progress(&TaskOperationProgress {
                    task_id: "task".into(),
                    stage: TaskOperationStage::Validating,
                    completed_steps: 2,
                    total_steps: 4,
                });
                control.report_progress(&TaskOperationProgress {
                    task_id: "another-task".into(),
                    stage: TaskOperationStage::Complete,
                    completed_steps: 4,
                    total_steps: 4,
                });
                Ok(())
            },
        )
        .unwrap();

    let finished = wait_until_terminal(&registry, &started.operation_id);
    assert_eq!(finished.status, ProjectTaskOperationStatus::Succeeded);
    assert_eq!(
        finished.progress.unwrap().stage,
        TaskOperationStage::Finalizing
    );
    assert_eq!(finished.project_root, PathBuf::from("/project"));
}

#[test]
fn cancellation_is_cooperative_and_idempotently_observable() {
    let registry = ProjectTaskOperationRegistry::default();
    let started = registry
        .start_operation(
            "project".into(),
            PathBuf::from("/project"),
            "cancel-me".into(),
            ProjectTaskOperationKind::Cleanup,
            |control| loop {
                if control.is_cancelled() {
                    break Err(ProjectError::new(
                        "project_task_operation_cancelled",
                        "cancelled",
                    ));
                }
                std::thread::yield_now();
            },
        )
        .unwrap();

    let requested = registry.cancel(&started.operation_id).unwrap();
    assert!(requested.cancellation_requested);
    let finished = wait_until_terminal(&registry, &started.operation_id);
    assert_eq!(finished.status, ProjectTaskOperationStatus::Cancelled);
    assert!(finished.cancellation_requested);
    assert_eq!(registry.cancel(&started.operation_id).unwrap(), finished);
}

#[test]
fn a_task_accepts_only_one_active_lifecycle_operation() {
    let registry = ProjectTaskOperationRegistry::default();
    let started = registry
        .start_operation(
            "project".into(),
            PathBuf::from("/project"),
            "exclusive".into(),
            ProjectTaskOperationKind::Provision,
            |control| loop {
                if control.is_cancelled() {
                    break Err(ProjectError::new(
                        "project_task_operation_cancelled",
                        "cancelled",
                    ));
                }
                std::thread::yield_now();
            },
        )
        .unwrap();
    let error = registry
        .start_operation(
            "project".into(),
            PathBuf::from("/project"),
            "exclusive".into(),
            ProjectTaskOperationKind::Cleanup,
            |_| Ok(()),
        )
        .unwrap_err();

    assert_eq!(error.code, "project_task_operation_active");
    registry.cancel(&started.operation_id).unwrap();
    assert_eq!(
        wait_until_terminal(&registry, &started.operation_id).status,
        ProjectTaskOperationStatus::Cancelled
    );
}

#[test]
fn shutdown_cancels_active_work_and_refuses_new_operations() {
    let registry = ProjectTaskOperationRegistry::default();
    let started = registry
        .start_operation(
            "project".into(),
            PathBuf::from("/project"),
            "shutdown".into(),
            ProjectTaskOperationKind::Provision,
            |control| loop {
                if control.is_cancelled() {
                    break Err(ProjectError::new(
                        "project_task_operation_cancelled",
                        "cancelled",
                    ));
                }
                std::thread::yield_now();
            },
        )
        .unwrap();

    registry.cancel_all();
    assert_eq!(
        wait_until_terminal(&registry, &started.operation_id).status,
        ProjectTaskOperationStatus::Cancelled
    );
    let error = registry
        .start_operation(
            "project".into(),
            PathBuf::from("/project"),
            "after-shutdown".into(),
            ProjectTaskOperationKind::Cleanup,
            |_| Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.code, "project_task_operations_shutting_down");
}

#[test]
fn worker_panics_become_terminal_failures_and_release_task_exclusivity() {
    let registry = ProjectTaskOperationRegistry::default();
    let failed = registry
        .start_operation(
            "project".into(),
            PathBuf::from("/project"),
            "panic".into(),
            ProjectTaskOperationKind::Provision,
            |_| panic!("synthetic worker panic"),
        )
        .unwrap();

    let finished = wait_until_terminal(&registry, &failed.operation_id);
    assert_eq!(finished.status, ProjectTaskOperationStatus::Failed);
    assert_eq!(
        finished.error.unwrap().code,
        "project_task_operation_panicked"
    );
    let retry = registry
        .start_operation(
            "project".into(),
            PathBuf::from("/project"),
            "panic".into(),
            ProjectTaskOperationKind::Cleanup,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(
        wait_until_terminal(&registry, &retry.operation_id).status,
        ProjectTaskOperationStatus::Succeeded
    );
}

#[test]
fn operation_ids_are_validated_before_lookup() {
    let error = ProjectTaskOperationRegistry::default()
        .get("../escape")
        .unwrap_err();
    assert_eq!(error.code, "invalid_project_task_operation_id");
}

#[test]
fn registry_prunes_the_oldest_terminal_operation_at_its_bound() {
    let registry = ProjectTaskOperationRegistry::default();
    {
        let mut state = registry.lock_state().unwrap();
        for sequence in 0..MAX_RETAINED_OPERATIONS as u64 {
            let operation_id = format!("retained-{sequence}");
            state.operations.insert(
                operation_id.clone(),
                test_entry(
                    operation_id,
                    sequence,
                    ProjectTaskOperationStatus::Succeeded,
                ),
            );
        }
    }

    let started = registry
        .start_operation(
            "project".into(),
            PathBuf::from("/project"),
            "replacement".into(),
            ProjectTaskOperationKind::Provision,
            |_| Ok(()),
        )
        .unwrap();
    wait_until_terminal(&registry, &started.operation_id);

    assert_eq!(
        registry.lock_state().unwrap().operations.len(),
        MAX_RETAINED_OPERATIONS
    );
    assert_eq!(
        registry.get("retained-0").unwrap_err().code,
        "project_task_operation_not_found"
    );
}

#[test]
fn registry_refuses_to_discard_active_operations_at_capacity() {
    let registry = ProjectTaskOperationRegistry::default();
    {
        let mut state = registry.lock_state().unwrap();
        for sequence in 0..MAX_RETAINED_OPERATIONS as u64 {
            let operation_id = format!("active-{sequence}");
            state.operations.insert(
                operation_id.clone(),
                test_entry(operation_id, sequence, ProjectTaskOperationStatus::Running),
            );
        }
    }

    let error = registry
        .start_operation(
            "project".into(),
            PathBuf::from("/project"),
            "overflow".into(),
            ProjectTaskOperationKind::Cleanup,
            |_| Ok(()),
        )
        .unwrap_err();
    assert_eq!(error.code, "project_task_operation_capacity");
    assert_eq!(
        registry.lock_state().unwrap().operations.len(),
        MAX_RETAINED_OPERATIONS
    );
}

fn test_entry(
    operation_id: String,
    sequence: u64,
    status: ProjectTaskOperationStatus,
) -> Arc<OperationEntry> {
    Arc::new(OperationEntry {
        record: Mutex::new(OperationRecord {
            sequence,
            snapshot: ProjectTaskOperationSnapshot {
                operation_id,
                project_id: "project".into(),
                project_root: PathBuf::from("/project"),
                task_id: "task".into(),
                kind: ProjectTaskOperationKind::Provision,
                status,
                cancellation_requested: false,
                progress: None,
                error: None,
            },
        }),
        cancelled: AtomicBool::new(false),
    })
}

fn wait_until_terminal(
    registry: &ProjectTaskOperationRegistry,
    operation_id: &str,
) -> ProjectTaskOperationSnapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = registry.get(operation_id).unwrap();
        if snapshot.status.is_terminal() {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "operation did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}
