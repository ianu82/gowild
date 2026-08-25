use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::provision::TaskWorkspaceProvisioner;
use super::provision_tests::ProjectFixture;
use super::{TaskOperationControl, TaskOperationProgress, TaskOperationStage, TaskWorkspacePhase};

#[test]
fn provisioning_cancels_before_the_next_resource_and_resumes_from_durable_state() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("controlled-provision");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let control = RecordingControl::cancel_at_repository("api");

    let error = provisioner
        .provision_with_control(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "controlled-provision",
            &control,
        )
        .unwrap_err();

    assert_eq!(error.code, "project_task_operation_cancelled");
    let interrupted = fixture.states.load("controlled-provision").unwrap();
    assert_eq!(interrupted.phase, TaskWorkspacePhase::Provisioning);
    assert!(interrupted.repositories["shared"].worktree.is_some());
    assert!(interrupted.repositories["api"].worktree.is_none());
    assert!(interrupted.repositories["web"].worktree.is_none());
    assert!(interrupted.root.exists());

    let resumed_control = RecordingControl::default();
    let ready = provisioner
        .provision_with_control(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "controlled-provision",
            &resumed_control,
        )
        .unwrap();
    assert_eq!(ready.phase, TaskWorkspacePhase::Ready);
    assert!(ready
        .repositories
        .values()
        .all(|repository| repository.worktree.is_some()));
    let progress = resumed_control.progress.lock().unwrap();
    assert_eq!(
        progress.first().unwrap().stage,
        TaskOperationStage::Validating
    );
    assert_eq!(progress.last().unwrap().stage, TaskOperationStage::Complete);
    assert!(progress
        .windows(2)
        .all(|window| window[0].completed_steps <= window[1].completed_steps));
    assert!(progress
        .iter()
        .all(|entry| entry.total_steps == ready.repositories.len() + 3));
    assert_eq!(
        progress.last().unwrap().completed_steps,
        progress.last().unwrap().total_steps
    );
}

#[test]
fn provisioning_cancelled_before_validation_does_not_mutate_task_state() {
    let fixture = ProjectFixture::new(false);
    let original = fixture.create_task("cancel-before-start");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let control = RecordingControl {
        cancelled: AtomicBool::new(true),
        ..RecordingControl::default()
    };

    let error = provisioner
        .provision_with_control(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "cancel-before-start",
            &control,
        )
        .unwrap_err();

    assert_eq!(error.code, "project_task_operation_cancelled");
    assert_eq!(
        fixture.states.load("cancel-before-start").unwrap(),
        original
    );
    assert!(control.progress.lock().unwrap().is_empty());
}

#[test]
fn controlled_provisioning_fails_fast_when_another_operation_owns_the_task() {
    let fixture = ProjectFixture::new(false);
    let original = fixture.create_task("busy-provision");
    let _operation_lock = fixture
        .states
        .lock_task_operations("busy-provision")
        .unwrap();
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let control = RecordingControl::default();

    let error = provisioner
        .provision_with_control(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "busy-provision",
            &control,
        )
        .unwrap_err();

    assert_eq!(error.code, "task_workspace_busy");
    assert_eq!(fixture.states.load("busy-provision").unwrap(), original);
    assert!(control.progress.lock().unwrap().is_empty());
}

#[derive(Default)]
struct RecordingControl {
    cancelled: AtomicBool,
    cancel_repository: Option<String>,
    progress: Mutex<Vec<TaskOperationProgress>>,
}

impl RecordingControl {
    fn cancel_at_repository(repository_id: &str) -> Self {
        Self {
            cancel_repository: Some(repository_id.to_string()),
            ..Self::default()
        }
    }
}

impl TaskOperationControl for RecordingControl {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn report_progress(&self, progress: &TaskOperationProgress) {
        self.progress.lock().unwrap().push(progress.clone());
        if matches!(
            &progress.stage,
            TaskOperationStage::Repository { repository_id }
                if self.cancel_repository.as_deref() == Some(repository_id.as_str())
        ) {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}
