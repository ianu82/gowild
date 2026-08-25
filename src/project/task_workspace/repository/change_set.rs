use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{
    ensure_regular_destination, read_bounded_regular_file, restrict_file_permissions,
    task_io_error, TaskWorkspaceRepository, MAX_TASK_STATE_BYTES,
};
use crate::project::change_set::ChangeSet;
use crate::project::private_state::write::{atomic_owner_only_write, PrivateWriteMode};
use crate::project::task_workspace::{validate_identifier, TaskWorkspace};
use crate::project::ProjectError;

pub const CHANGE_SET_STATE_VERSION: u32 = 1;
const CHANGE_SET_FILE_PREFIX: &str = ".change-set-";
const CHANGE_SET_FILE_SUFFIX: &str = ".json";

/// One optimistic, owner-only durable snapshot of a task's coordinated changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSetRecord {
    pub schema_version: u32,
    pub revision: u64,
    pub change_set: ChangeSet,
}

impl TaskWorkspaceRepository {
    pub fn change_set_path(&self, task_id: &str) -> Result<PathBuf, ProjectError> {
        validate_identifier("task id", task_id)?;
        Ok(self.state_root.join(format!(
            "{CHANGE_SET_FILE_PREFIX}{task_id}{CHANGE_SET_FILE_SUFFIX}"
        )))
    }

    pub fn load_change_set(
        &self,
        task: &TaskWorkspace,
    ) -> Result<Option<ChangeSetRecord>, ProjectError> {
        let current_task = self.require_current_task(task)?;
        let record = self.load_change_set_record(&current_task.id)?;
        if let Some(record) = &record {
            validate_record(record, &current_task)?;
        }
        Ok(record)
    }

    /// Atomically creates or replaces one change-set revision.
    ///
    /// `None` is accepted only for the initial record. Exact same-content
    /// retries are idempotent, including after the caller lost the response.
    pub fn save_change_set(
        &self,
        task: &TaskWorkspace,
        change_set: &ChangeSet,
        expected_revision: Option<u64>,
    ) -> Result<ChangeSetRecord, ProjectError> {
        task.validate_integrity()?;
        change_set.validate_for_task(task)?;
        self.validate_existing_state_root()?;
        let _store_lock = self.lock_store()?;
        let current_task = self.require_current_task(task)?;
        change_set.validate_for_task(&current_task)?;
        require_current_snapshot(change_set, &current_task)?;
        let current = self.load_change_set_record(&task.id)?;
        if let Some(current) = &current {
            validate_record(current, &current_task)?;
            if current.change_set == *change_set {
                return Ok(current.clone());
            }
        }

        let revision = next_revision(current.as_ref(), expected_revision)?;
        let record = ChangeSetRecord {
            schema_version: CHANGE_SET_STATE_VERSION,
            revision,
            change_set: change_set.clone(),
        };
        let bytes = serialize_record(&record)?;
        let path = self.change_set_path(&task.id)?;
        let mode = if current.is_some() {
            ensure_regular_destination(&path)?;
            PrivateWriteMode::Replace
        } else {
            PrivateWriteMode::CreateNew
        };
        atomic_owner_only_write(&path, &bytes, mode)
            .map_err(|error| task_io_error("change-set save", &error))?;
        Ok(record)
    }

    fn require_current_task(
        &self,
        expected: &TaskWorkspace,
    ) -> Result<TaskWorkspace, ProjectError> {
        expected.validate_integrity()?;
        let current = self.load(&expected.id)?;
        if current == *expected {
            Ok(current)
        } else {
            Err(ProjectError::new(
                "task_change_set_task_stale",
                "task workspace changed before its change set could be persisted",
            ))
        }
    }

    fn load_change_set_record(
        &self,
        task_id: &str,
    ) -> Result<Option<ChangeSetRecord>, ProjectError> {
        let path = self.change_set_path(task_id)?;
        let Some(bytes) =
            read_bounded_regular_file(&path, MAX_TASK_STATE_BYTES).map_err(|error| {
                if error.code == "invalid_task_workspace_state" {
                    invalid_record()
                } else {
                    error
                }
            })?
        else {
            return Ok(None);
        };
        let record = serde_json::from_slice::<ChangeSetRecord>(&bytes).map_err(|_| {
            ProjectError::new(
                "invalid_task_change_set_state",
                "task change-set state is invalid JSON",
            )
        })?;
        restrict_file_permissions(&path)?;
        Ok(Some(record))
    }
}

pub(super) fn state_file_task_id(name: &str) -> Option<&str> {
    let task_id = name
        .strip_prefix(CHANGE_SET_FILE_PREFIX)?
        .strip_suffix(CHANGE_SET_FILE_SUFFIX)?;
    validate_identifier("task id", task_id)
        .is_ok()
        .then_some(task_id)
}

fn validate_record(record: &ChangeSetRecord, task: &TaskWorkspace) -> Result<(), ProjectError> {
    if record.schema_version != CHANGE_SET_STATE_VERSION {
        return Err(ProjectError::new(
            "unsupported_task_change_set_state_version",
            format!(
                "task change-set state version {} is not supported",
                record.schema_version
            ),
        ));
    }
    record.change_set.validate_for_task(task)
}

fn require_current_snapshot(
    change_set: &ChangeSet,
    task: &TaskWorkspace,
) -> Result<(), ProjectError> {
    if change_set.is_stale_for_task(task) {
        Err(ProjectError::new(
            "task_change_set_snapshot_stale",
            "task workspace changed after this change-set snapshot was collected",
        ))
    } else {
        Ok(())
    }
}

fn next_revision(
    current: Option<&ChangeSetRecord>,
    expected: Option<u64>,
) -> Result<u64, ProjectError> {
    match (current, expected) {
        (None, None) => Ok(0),
        (Some(current), Some(expected)) if current.revision == expected => {
            current.revision.checked_add(1).ok_or_else(|| {
                ProjectError::new(
                    "task_change_set_revision_exhausted",
                    "task change-set revision counter is exhausted",
                )
            })
        }
        _ => Err(ProjectError::new(
            "task_change_set_revision_conflict",
            "task change-set state changed before this update",
        )),
    }
}

fn serialize_record(record: &ChangeSetRecord) -> Result<Vec<u8>, ProjectError> {
    let bytes = serde_json::to_vec_pretty(record).map_err(|_| {
        ProjectError::new(
            "invalid_task_change_set_state",
            "could not serialize task change-set state",
        )
    })?;
    if bytes.len() as u64 > MAX_TASK_STATE_BYTES {
        return Err(ProjectError::new(
            "task_change_set_state_too_large",
            "task change-set state exceeds 16 MiB",
        ));
    }
    Ok(bytes)
}

fn invalid_record() -> ProjectError {
    ProjectError::new(
        "invalid_task_change_set_state",
        "task change-set state must be a bounded regular file, not a symlink",
    )
}
