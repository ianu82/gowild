use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{SystemTime, UNIX_EPOCH};

use super::repository::TaskWorkspaceRepository;
use super::tests::{loaded_project, route};
use super::*;

static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

fn test_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed);
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir());
    temp_root.join(format!(
        "gowild-{label}-{}-{nonce}-{sequence}",
        std::process::id()
    ))
}

fn repository(parent: &Path) -> TaskWorkspaceRepository {
    TaskWorkspaceRepository::new(parent.join("state"), parent.join("workspaces"))
}

fn task(repository: &TaskWorkspaceRepository, task_id: &str) -> TaskWorkspace {
    TaskWorkspace::new(
        &loaded_project(),
        task_id,
        "Update the API and web client",
        TaskAgent::Codex,
        route(),
        repository.workspace_store_root().to_path_buf(),
    )
    .unwrap()
}

#[test]
fn repository_creates_loads_and_lists_strict_task_state() {
    let parent = test_root("task-state-round-trip");
    let repository = repository(&parent);
    let task = task(&repository, "task-42");

    repository.create(&task).unwrap();
    assert_eq!(repository.load("task-42").unwrap(), task);
    assert_eq!(repository.list_ids().unwrap(), vec!["task-42"]);
    repository.create(&task).unwrap();
    let mut conflicting = task.clone();
    conflicting.outcome = "A conflicting task definition".into();
    assert_eq!(
        repository.create(&conflicting).unwrap_err().code,
        "task_workspace_already_exists"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let state_path = repository.state_path("task-42").unwrap();
        assert_eq!(
            std::fs::metadata(state_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(parent.join("state"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn repository_recovers_an_interrupted_ownership_marker_commit() {
    let parent = test_root("task-state-marker-recovery");
    let repository = repository(&parent);
    std::fs::create_dir_all(parent.join("state")).unwrap();
    std::fs::write(
        parent.join("state/.project-state.123.456.789.tmp"),
        b"gowild task workspace state v1\n",
    )
    .unwrap();

    repository
        .create(&task(&repository, "recovered-task"))
        .unwrap();
    assert_eq!(repository.list_ids().unwrap(), vec!["recovered-task"]);
    assert!(!parent.join("state/.project-state.123.456.789.tmp").exists());

    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn repository_recovers_a_partial_interrupted_ownership_marker_write() {
    let parent = test_root("task-state-partial-marker-recovery");
    let repository = repository(&parent);
    std::fs::create_dir_all(parent.join("state")).unwrap();
    let interrupted = parent.join("state/.project-state.123.456.789.tmp");
    std::fs::write(&interrupted, b"gowild task work").unwrap();

    repository
        .create(&task(&repository, "recovered-partial-task"))
        .unwrap();
    assert_eq!(
        repository.list_ids().unwrap(),
        vec!["recovered-partial-task"]
    );
    assert_eq!(std::fs::read(interrupted).unwrap(), b"gowild task work");

    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn repository_ignores_reserved_atomic_writer_files_after_ownership_is_proven() {
    let parent = test_root("task-state-writer-temp");
    let repository = repository(&parent);
    repository
        .create(&task(&repository, "stable-task"))
        .unwrap();
    std::fs::write(
        parent.join("state/.project-state.123.456.789.tmp"),
        b"interrupted next revision",
    )
    .unwrap();

    assert_eq!(repository.list_ids().unwrap(), vec!["stable-task"]);

    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn repository_serializes_concurrent_revision_updates() {
    let parent = test_root("task-state-concurrent-save");
    let repository = Arc::new(repository(&parent));
    let initial = task(&repository, "concurrent-task");
    repository.create(&initial).unwrap();

    let mut provisioning = initial.clone();
    provisioning
        .transition_phase(TaskWorkspacePhase::Provisioning)
        .unwrap();
    let mut cleaning = initial;
    cleaning
        .transition_phase(TaskWorkspacePhase::Cleaning)
        .unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let first_repository = Arc::clone(&repository);
    let first_barrier = Arc::clone(&barrier);
    let first = std::thread::spawn(move || {
        first_barrier.wait();
        let result = first_repository.save(&provisioning, 0);
        (provisioning, result)
    });
    let second_repository = Arc::clone(&repository);
    let second_barrier = Arc::clone(&barrier);
    let second = std::thread::spawn(move || {
        second_barrier.wait();
        let result = second_repository.save(&cleaning, 0);
        (cleaning, result)
    });
    barrier.wait();

    let outcomes = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(
        outcomes.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter_map(|(_, result)| result.as_ref().err())
            .map(|error| error.code)
            .collect::<Vec<_>>(),
        vec!["task_workspace_revision_conflict"]
    );
    let winner = outcomes
        .iter()
        .find(|(_, result)| result.is_ok())
        .map(|(task, _)| task)
        .unwrap();
    assert_eq!(repository.load("concurrent-task").unwrap(), *winner);

    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn repository_operation_lock_is_per_task_and_crash_released() {
    let parent = test_root("task-operation-lock");
    let repository = repository(&parent);
    repository.create(&task(&repository, "task-one")).unwrap();
    repository.create(&task(&repository, "task-two")).unwrap();

    let task_one_lock = repository.lock_task_operations("task-one").unwrap();
    assert_eq!(
        repository
            .try_lock_task_operations("task-one")
            .unwrap_err()
            .code,
        "task_workspace_busy"
    );
    let task_two_lock = repository.try_lock_task_operations("task-two").unwrap();
    assert_eq!(repository.list_ids().unwrap(), vec!["task-one", "task-two"]);

    drop(task_one_lock);
    repository.try_lock_task_operations("task-one").unwrap();
    drop(task_two_lock);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(parent.join("state/.task-operation-task-one.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let _ = std::fs::remove_dir_all(parent);
}

#[cfg(unix)]
#[test]
fn repository_rejects_a_symlinked_operation_lock() {
    use std::os::unix::fs::symlink;

    let parent = test_root("task-operation-lock-symlink");
    let repository = repository(&parent);
    repository
        .create(&task(&repository, "linked-task"))
        .unwrap();
    let target = parent.join("outside.lock");
    std::fs::write(&target, b"do not lock").unwrap();
    symlink(
        &target,
        parent.join("state/.task-operation-linked-task.lock"),
    )
    .unwrap();

    assert_eq!(
        repository
            .try_lock_task_operations("linked-task")
            .unwrap_err()
            .code,
        "invalid_task_workspace_operation_lock"
    );
    assert_eq!(std::fs::read(target).unwrap(), b"do not lock");

    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn repository_requires_exactly_one_revision_per_durable_save() {
    let parent = test_root("task-state-revisions");
    let repository = repository(&parent);
    let initial = task(&repository, "revision-task");
    repository.create(&initial).unwrap();

    let mut provisioning = initial.clone();
    provisioning
        .transition_phase(TaskWorkspacePhase::Provisioning)
        .unwrap();
    repository.save(&provisioning, 0).unwrap();
    assert_eq!(repository.load("revision-task").unwrap(), provisioning);
    repository.save(&provisioning, 0).unwrap();

    let mut stale = initial;
    stale
        .transition_phase(TaskWorkspacePhase::Cleaning)
        .unwrap();
    assert_eq!(
        repository.save(&stale, 0).unwrap_err().code,
        "task_workspace_revision_conflict"
    );

    let mut skipped = provisioning;
    skipped
        .transition_phase(TaskWorkspacePhase::NeedsAttention)
        .unwrap();
    skipped
        .transition_phase(TaskWorkspacePhase::Provisioning)
        .unwrap();
    assert_eq!(
        repository.save(&skipped, 1).unwrap_err().code,
        "invalid_task_workspace_revision"
    );

    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn repository_rejects_corrupt_and_oversized_state_before_deserialization() {
    let parent = test_root("task-state-corrupt");
    let repository = repository(&parent);
    repository
        .create(&task(&repository, "corrupt-task"))
        .unwrap();
    let path = repository.state_path("corrupt-task").unwrap();

    std::fs::write(&path, b"{not-json").unwrap();
    assert_eq!(
        repository.load("corrupt-task").unwrap_err().code,
        "invalid_task_workspace_state"
    );

    std::fs::File::create(&path)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        repository.load("corrupt-task").unwrap_err().code,
        "invalid_task_workspace_state"
    );

    let _ = std::fs::remove_dir_all(parent);
}

#[cfg(unix)]
#[test]
fn repository_rejects_symlinked_state_and_store_paths() {
    use std::os::unix::fs::symlink;

    let parent = test_root("task-state-symlinks");
    let repository = repository(&parent);
    repository
        .create(&task(&repository, "linked-task"))
        .unwrap();
    let state_path = repository.state_path("linked-task").unwrap();
    let target = parent.join("outside.json");
    std::fs::write(&target, b"{}").unwrap();
    std::fs::remove_file(&state_path).unwrap();
    symlink(&target, &state_path).unwrap();
    assert_eq!(
        repository.load("linked-task").unwrap_err().code,
        "invalid_task_workspace_state"
    );

    let linked_parent = test_root("task-store-symlink");
    std::fs::create_dir_all(&linked_parent).unwrap();
    let outside_state = linked_parent.join("outside-state");
    std::fs::create_dir(&outside_state).unwrap();
    let linked_state = linked_parent.join("state");
    symlink(&outside_state, &linked_state).unwrap();
    let linked_repository =
        TaskWorkspaceRepository::new(linked_state, linked_parent.join("workspaces"));
    assert_eq!(
        linked_repository
            .create(&task(&linked_repository, "blocked-task"))
            .unwrap_err()
            .code,
        "invalid_task_workspace_state_directory"
    );

    let _ = std::fs::remove_dir_all(parent);
    let _ = std::fs::remove_dir_all(linked_parent);
}

#[cfg(unix)]
#[test]
fn repository_rejects_symlinked_workspace_ancestors() {
    use std::os::unix::fs::symlink;

    let parent = test_root("task-workspace-symlink");
    std::fs::create_dir_all(&parent).unwrap();
    let outside = parent.join("outside-workspaces");
    std::fs::create_dir(&outside).unwrap();
    let workspace_root = parent.join("workspaces");
    symlink(&outside, &workspace_root).unwrap();
    let repository = TaskWorkspaceRepository::new(parent.join("state"), workspace_root);

    assert_eq!(
        repository
            .create(&task(&repository, "escaped-task"))
            .unwrap_err()
            .code,
        "invalid_task_workspace_store"
    );

    let _ = std::fs::remove_dir_all(parent);
}

#[test]
fn repository_refuses_to_adopt_nonempty_unmarked_directories() {
    let parent = test_root("task-state-unmarked");
    let repository = repository(&parent);
    std::fs::create_dir_all(parent.join("state")).unwrap();
    std::fs::write(parent.join("state/user-file"), b"preserve me").unwrap();

    assert_eq!(
        repository
            .create(&task(&repository, "safe-task"))
            .unwrap_err()
            .code,
        "invalid_task_workspace_state_directory"
    );
    assert_eq!(
        std::fs::read(parent.join("state/user-file")).unwrap(),
        b"preserve me"
    );

    let _ = std::fs::remove_dir_all(parent);
}
