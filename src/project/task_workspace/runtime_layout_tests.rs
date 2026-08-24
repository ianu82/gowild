use super::provision::{ensure_owned_task_root, TaskWorkspaceProvisioner};
use super::provision_tests::{persist_phase, persist_plan, ProjectFixture};
use super::runtime_layout::{
    ensure_owned_runtime_directory, ensure_runtime_directory_released, runtime_directories,
    runtime_directory_marker,
};
use super::*;

#[test]
fn provisioning_materializes_owned_private_runtime_directories_per_task() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("runtime-one");
    fixture.create_task("runtime-two");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);

    let first = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "runtime-one",
        )
        .unwrap();
    let second = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "runtime-two",
        )
        .unwrap();

    let first_paths = runtime_directories(&first);
    let second_paths = runtime_directories(&second);
    for (first_path, second_path) in first_paths.iter().zip(second_paths.iter()) {
        assert_ne!(first_path, second_path);
        assert!(first_path.is_dir());
        assert!(second_path.is_dir());
        assert!(runtime_directory_marker(&first, first_path)
            .unwrap()
            .is_dir());
        assert!(runtime_directory_marker(&second, second_path)
            .unwrap()
            .is_dir());
        assert!(first.resource_is_owned(&OwnedResource::RuntimeDirectory {
            path: first_path.clone(),
        }));
        assert_private_directory(first_path);
    }
    let acquired = first
        .journal
        .iter()
        .filter_map(|transition| match &transition.resource {
            OwnedResource::RuntimeDirectory { path }
                if transition.operation == TaskTransitionOperation::Acquire
                    && transition.state == TaskTransitionState::Applied =>
            {
                Some(path.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(acquired, first_paths);
}

#[test]
fn provisioning_reconciles_a_runtime_directory_created_after_its_durable_plan() {
    let fixture = ProjectFixture::new(false);
    let mut task = fixture.create_task("runtime-create-crash");
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Provisioning);
    acquire_task_root(&fixture, &mut task);
    let runtime_root = task.runtime.root.clone();
    let sequence = persist_plan(
        &fixture.states,
        &mut task,
        OwnedResource::RuntimeDirectory {
            path: runtime_root.clone(),
        },
    );
    ensure_owned_runtime_directory(&task, &runtime_root).unwrap();
    assert_eq!(task.journal.last().unwrap().sequence, sequence);
    assert_eq!(
        task.journal.last().unwrap().state,
        TaskTransitionState::Planned
    );

    let resumed = TaskWorkspaceProvisioner::new(&fixture.states)
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "runtime-create-crash",
        )
        .unwrap();

    assert_eq!(resumed.phase, TaskWorkspacePhase::Ready);
    assert!(runtime_directories(&resumed)
        .iter()
        .all(|path| path.is_dir()));
    assert!(resumed
        .journal
        .iter()
        .all(|transition| transition.state != TaskTransitionState::Planned));
}

#[test]
fn provisioning_preserves_an_unowned_runtime_directory_and_records_attention() {
    let fixture = ProjectFixture::new(false);
    let mut task = fixture.create_task("runtime-conflict");
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Provisioning);
    acquire_task_root(&fixture, &mut task);
    std::fs::create_dir(&task.runtime.root).unwrap();
    std::fs::write(task.runtime.root.join("keep.txt"), b"unowned data\n").unwrap();

    let error = TaskWorkspaceProvisioner::new(&fixture.states)
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "runtime-conflict",
        )
        .unwrap_err();

    assert_eq!(error.code, "task_runtime_ownership_mismatch");
    assert_eq!(
        std::fs::read(task.runtime.root.join("keep.txt")).unwrap(),
        b"unowned data\n"
    );
    let persisted = fixture.states.load("runtime-conflict").unwrap();
    assert_eq!(persisted.phase, TaskWorkspacePhase::NeedsAttention);
    assert_eq!(
        persisted.journal.last().unwrap().failure_code.as_deref(),
        Some("task_runtime_ownership_mismatch")
    );
}

#[cfg(unix)]
#[test]
fn provisioning_refuses_a_symlinked_runtime_boundary() {
    use std::os::unix::fs::symlink;

    let fixture = ProjectFixture::new(false);
    let mut task = fixture.create_task("runtime-symlink");
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Provisioning);
    acquire_task_root(&fixture, &mut task);
    let outside = fixture.root.join("outside-runtime");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"outside\n").unwrap();
    symlink(&outside, &task.runtime.root).unwrap();

    let error = TaskWorkspaceProvisioner::new(&fixture.states)
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "runtime-symlink",
        )
        .unwrap_err();

    assert!(matches!(
        error.code,
        "invalid_task_workspace_store" | "invalid_task_workspace_state_directory"
    ));
    assert_eq!(
        std::fs::read(outside.join("keep.txt")).unwrap(),
        b"outside\n"
    );
}

#[test]
fn cleanup_removes_owned_runtime_contents_and_is_idempotent() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("runtime-cleanup");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let provisioned = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "runtime-cleanup",
        )
        .unwrap();
    std::fs::write(provisioned.runtime.temp.join("scratch"), b"temporary\n").unwrap();
    std::fs::write(provisioned.runtime.cache.join("artifact"), b"cache\n").unwrap();
    std::fs::create_dir_all(provisioned.runtime.data.join("databases/api")).unwrap();
    std::fs::write(
        provisioned.runtime.data.join("databases/api/state"),
        b"task data\n",
    )
    .unwrap();

    let cleaned = provisioner.cleanup("runtime-cleanup").unwrap();

    assert_eq!(cleaned.phase, TaskWorkspacePhase::Cleaned);
    assert!(!cleaned.root.exists());
    let released = cleaned
        .journal
        .iter()
        .filter_map(|transition| match &transition.resource {
            OwnedResource::RuntimeDirectory { path }
                if transition.operation == TaskTransitionOperation::Release
                    && transition.state == TaskTransitionState::Applied =>
            {
                Some(path.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        released,
        runtime_directories(&provisioned)
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
    );
    assert_eq!(provisioner.cleanup("runtime-cleanup").unwrap(), cleaned);
}

#[test]
fn cleanup_reconciles_a_runtime_directory_removed_after_its_durable_plan() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("runtime-cleanup-crash");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let mut task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "runtime-cleanup-crash",
        )
        .unwrap();
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Cleaning);
    let data = task.runtime.data.clone();
    let expected_revision = task.revision;
    task.plan_transition(
        TaskTransitionOperation::Release,
        OwnedResource::RuntimeDirectory { path: data.clone() },
    )
    .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    ensure_runtime_directory_released(&task, &data).unwrap();

    let recovered = provisioner.cleanup("runtime-cleanup-crash").unwrap();

    assert_eq!(recovered.phase, TaskWorkspacePhase::Cleaned);
    assert!(!recovered.root.exists());
    assert!(recovered
        .journal
        .iter()
        .all(|transition| transition.state != TaskTransitionState::Planned));
}

#[test]
fn cleanup_retries_a_runtime_release_recorded_failed_after_external_completion() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("runtime-cleanup-failed");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let mut task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "runtime-cleanup-failed",
        )
        .unwrap();
    persist_phase(&fixture.states, &mut task, TaskWorkspacePhase::Cleaning);
    let data = task.runtime.data.clone();
    let expected_revision = task.revision;
    let sequence = task
        .plan_transition(
            TaskTransitionOperation::Release,
            OwnedResource::RuntimeDirectory { path: data.clone() },
        )
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    ensure_runtime_directory_released(&task, &data).unwrap();
    provisioner
        .record_failed_transition(&mut task, sequence, "simulated_runtime_cleanup_failure")
        .unwrap();

    let recovered = provisioner.cleanup("runtime-cleanup-failed").unwrap();

    assert_eq!(recovered.phase, TaskWorkspacePhase::Cleaned);
    assert!(!recovered.root.exists());
}

#[test]
fn cleanup_preserves_runtime_data_when_its_ownership_marker_is_missing() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("runtime-marker-missing");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "runtime-marker-missing",
        )
        .unwrap();
    let keep = task.runtime.data.join("keep.txt");
    std::fs::write(&keep, b"preserve me\n").unwrap();
    std::fs::remove_dir(runtime_directory_marker(&task, &task.runtime.data).unwrap()).unwrap();

    let error = provisioner.cleanup("runtime-marker-missing").unwrap_err();

    assert_eq!(error.code, "task_runtime_ownership_mismatch");
    assert_eq!(std::fs::read(&keep).unwrap(), b"preserve me\n");
    assert_eq!(
        fixture.states.load("runtime-marker-missing").unwrap().phase,
        TaskWorkspacePhase::Ready
    );
}

fn acquire_task_root(fixture: &ProjectFixture, task: &mut TaskWorkspace) {
    let sequence = persist_plan(
        &fixture.states,
        task,
        OwnedResource::WorkspaceDirectory {
            path: task.root.clone(),
        },
    );
    ensure_owned_task_root(task).unwrap();
    let expected_revision = task.revision;
    task.finish_transition(sequence, TaskTransitionState::Applied, None)
        .unwrap();
    fixture.states.save(task, expected_revision).unwrap();
}

#[cfg(unix)]
fn assert_private_directory(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[cfg(not(unix))]
fn assert_private_directory(_path: &std::path::Path) {}
