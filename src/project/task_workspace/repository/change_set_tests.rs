use std::sync::{Arc, Barrier};

use super::*;
use crate::project::change_set::{ChangeSet, ChangeSetCheck, CheckStatus};
use crate::project::task_workspace::provision::TaskWorkspaceProvisioner;
use crate::project::task_workspace::provision_tests::ProjectFixture;
use crate::project::task_workspace::TaskWorkspacePhase;

#[test]
fn change_set_state_round_trips_updates_and_stays_out_of_task_listing() {
    let (fixture, task, change_set) = fixture("durable");

    let created = fixture
        .states
        .save_change_set(&task, &change_set, None)
        .unwrap();
    assert_eq!(created.revision, 0);
    assert_eq!(
        fixture.states.load_change_set(&task).unwrap(),
        Some(created.clone())
    );
    assert_eq!(fixture.states.list_ids().unwrap(), vec!["durable"]);

    let mut updated = change_set;
    updated
        .checks
        .insert("check".into(), passed_check("check", 5));
    let saved = fixture
        .states
        .save_change_set(&task, &updated, Some(0))
        .unwrap();
    assert_eq!(saved.revision, 1);
    assert_eq!(
        fixture
            .states
            .save_change_set(&task, &updated, Some(0))
            .unwrap(),
        saved
    );

    let mut running = task;
    running
        .transition_phase(TaskWorkspacePhase::Running)
        .unwrap();
    fixture.states.save(&running, running.revision - 1).unwrap();
    let loaded = fixture.states.load_change_set(&running).unwrap().unwrap();
    assert!(loaded.change_set.is_stale_for_task(&running));
    assert_eq!(
        fixture
            .states
            .save_change_set(&running, &loaded.change_set, Some(loaded.revision))
            .unwrap_err()
            .code,
        "task_change_set_snapshot_stale"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = fixture.states.change_set_path("durable").unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn change_set_state_detects_task_and_record_revision_conflicts() {
    let (fixture, task, change_set) = fixture("conflict");
    fixture
        .states
        .save_change_set(&task, &change_set, None)
        .unwrap();

    let mut stale_task = task.clone();
    stale_task
        .transition_phase(TaskWorkspacePhase::Running)
        .unwrap();
    assert_eq!(
        fixture
            .states
            .save_change_set(&stale_task, &change_set, Some(0))
            .unwrap_err()
            .code,
        "task_change_set_task_stale"
    );

    let mut changed = change_set;
    changed
        .checks
        .insert("check".into(), passed_check("check", 1));
    assert_eq!(
        fixture
            .states
            .save_change_set(&task, &changed, None)
            .unwrap_err()
            .code,
        "task_change_set_revision_conflict"
    );
}

#[test]
fn change_set_state_serializes_concurrent_updates() {
    let (fixture, task, change_set) = fixture("concurrent");
    fixture
        .states
        .save_change_set(&task, &change_set, None)
        .unwrap();
    let states = Arc::new(fixture.states.clone());
    let barrier = Arc::new(Barrier::new(3));
    let handles = [1_u64, 2_u64].map(|duration| {
        let states = Arc::clone(&states);
        let barrier = Arc::clone(&barrier);
        let task = task.clone();
        let mut candidate = change_set.clone();
        candidate
            .checks
            .insert("check".into(), passed_check("check", duration));
        std::thread::spawn(move || {
            barrier.wait();
            states.save_change_set(&task, &candidate, Some(0))
        })
    });
    barrier.wait();

    let results = handles.map(|handle| handle.join().unwrap());
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(|error| error.code)
            .collect::<Vec<_>>(),
        ["task_change_set_revision_conflict"]
    );
}

#[test]
fn change_set_state_rejects_symlinks_oversize_and_tampering() {
    let (fixture, task, change_set) = fixture("hostile");
    fixture
        .states
        .save_change_set(&task, &change_set, None)
        .unwrap();
    let path = fixture.states.change_set_path("hostile").unwrap();

    let mut json =
        serde_json::to_value(fixture.states.load_change_set(&task).unwrap().unwrap()).unwrap();
    json["change_set"]["repositories"]["api"]["checkout_path"] =
        serde_json::json!("/tmp/redirected");
    std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert_eq!(
        fixture.states.load_change_set(&task).unwrap_err().code,
        "invalid_task_change_set_state"
    );

    std::fs::write(&path, vec![b' '; (MAX_TASK_STATE_BYTES + 1) as usize]).unwrap();
    assert_eq!(
        fixture.states.load_change_set(&task).unwrap_err().code,
        "invalid_task_change_set_state"
    );

    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(&path);
        std::os::unix::fs::symlink("elsewhere", &path).unwrap();
        assert_eq!(
            fixture.states.load_change_set(&task).unwrap_err().code,
            "invalid_task_change_set_state"
        );
    }
}

fn fixture(task_id: &str) -> (ProjectFixture, TaskWorkspace, ChangeSet) {
    let fixture = ProjectFixture::new(false);
    fixture.create_task(task_id);
    let task = TaskWorkspaceProvisioner::new(&fixture.states)
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            task_id,
        )
        .unwrap();
    let change_set = ChangeSet::for_task(&task).unwrap();
    (fixture, task, change_set)
}

fn passed_check(command_id: &str, duration_ms: u64) -> ChangeSetCheck {
    ChangeSetCheck {
        command_id: command_id.into(),
        repository_id: None,
        status: CheckStatus::Passed,
        duration_ms: Some(duration_ms),
        exit_code: Some(0),
        failure_code: None,
    }
}
