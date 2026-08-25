use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::operation::TaskOperationProgress;
use super::provision::TaskWorkspaceProvisioner;
use super::provision_tests::ProjectFixture;
use super::{TaskOperationControl, TaskOperationStage, TaskWorkspacePhase};

#[test]
fn provisioning_cancels_before_the_next_resource_and_resumes_from_durable_state() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("controlled-provision");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let control = RecordingControl::cancel_at_stage(TaskOperationStage::Repository {
        repository_id: "api".into(),
    });

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

#[test]
fn cleanup_cancels_before_the_next_release_and_resumes_without_data_loss() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("controlled-cleanup");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let ready = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "controlled-cleanup",
        )
        .unwrap();
    let control = RecordingControl::cancel_at_stage(TaskOperationStage::RepositoryWorktree {
        repository_id: "api".into(),
    });

    let error = provisioner
        .cleanup_with_control("controlled-cleanup", &control)
        .unwrap_err();

    assert_eq!(error.code, "project_task_operation_cancelled");
    let interrupted = fixture.states.load("controlled-cleanup").unwrap();
    assert_eq!(interrupted.phase, TaskWorkspacePhase::Cleaning);
    assert!(!ready.repository_checkout_path("web").exists());
    assert!(ready.repository_checkout_path("api").exists());
    assert!(ready.repository_checkout_path("shared").exists());
    assert!(ready.root.exists());

    let resumed_control = RecordingControl::default();
    let cleaned = provisioner
        .cleanup_with_control("controlled-cleanup", &resumed_control)
        .unwrap();
    assert_eq!(cleaned.phase, TaskWorkspacePhase::Cleaned);
    assert!(!cleaned.root.exists());
    let progress = resumed_control.progress.lock().unwrap();
    assert_eq!(progress.last().unwrap().stage, TaskOperationStage::Complete);
    assert!(progress
        .windows(2)
        .all(|window| window[0].completed_steps <= window[1].completed_steps));
    assert_eq!(
        progress.last().unwrap().completed_steps,
        progress.last().unwrap().total_steps
    );
}

#[test]
fn controlled_cleanup_fails_fast_when_another_operation_owns_the_task() {
    let fixture = ProjectFixture::new(false);
    let original = fixture.create_task("busy-cleanup");
    let _operation_lock = fixture.states.lock_task_operations("busy-cleanup").unwrap();
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let control = RecordingControl::default();

    let error = provisioner
        .cleanup_with_control("busy-cleanup", &control)
        .unwrap_err();

    assert_eq!(error.code, "task_workspace_busy");
    assert_eq!(fixture.states.load("busy-cleanup").unwrap(), original);
    assert!(control.progress.lock().unwrap().is_empty());
}

#[derive(Default)]
struct RecordingControl {
    cancelled: AtomicBool,
    cancel_stage: Option<TaskOperationStage>,
    progress: Mutex<Vec<TaskOperationProgress>>,
}

impl RecordingControl {
    fn cancel_at_stage(stage: TaskOperationStage) -> Self {
        Self {
            cancel_stage: Some(stage),
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
        if self.cancel_stage.as_ref() == Some(&progress.stage) {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}
