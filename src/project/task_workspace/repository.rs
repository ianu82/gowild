use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::rules::{validate_absolute_clean_path, validate_identifier};
use super::{TaskWorkspace, TaskWorkspacePhase};
use crate::project::private_state::manifest_identity;
use crate::project::private_state::write::{atomic_owner_only_write, PrivateWriteMode};
use crate::project::{ProjectDefinition, ProjectError};

mod storage;

pub(super) use storage::{
    directory_is_empty, ensure_private_directory_chain, restrict_directory_permissions,
    validate_existing_ancestors,
};
use storage::{
    ensure_regular_destination, is_private_writer_temp_name, open_owner_only_lock_file,
    operation_lock_repository_id, operation_lock_task_id, read_bounded_regular_file,
    recover_interrupted_marker_create, restrict_file_permissions, serialize_task, task_io_error,
    validate_existing_directory_chain,
};

const MAX_TASK_STATE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TASKS_PER_PROJECT: usize = 10_000;
const MAX_STATE_DIRECTORY_ENTRIES: usize = 20_000;
const STORE_MARKER_FILE: &str = ".gowild-task-store-v1";
const STORE_MARKER_CONTENT: &[u8] = b"gowild task workspace state v1\n";
const OPERATION_LOCK_PREFIX: &str = ".task-operation-";
const OPERATION_LOCK_SUFFIX: &str = ".lock";
const REPOSITORY_LOCK_PREFIX: &str = ".repository-operation-";

/// An OS-backed exclusive lease for one task's external mutations.
///
/// The operating system releases the lease if GoWild exits or crashes. The
/// empty owner-only file remains as a stable rendezvous point and contains no
/// task data or credentials.
#[derive(Debug)]
pub struct TaskWorkspaceOperationLock {
    _file: fs::File,
}

/// Durable, owner-only task state for one canonical project manifest.
///
/// State and task worktrees have separate roots so cleanup can never remove the
/// only ownership record that proves which data-plane resources belong to it.
#[derive(Debug, Clone)]
pub struct TaskWorkspaceRepository {
    state_root: PathBuf,
    workspace_root: PathBuf,
}

impl TaskWorkspaceRepository {
    pub fn new(state_root: PathBuf, workspace_root: PathBuf) -> Self {
        Self {
            state_root,
            workspace_root,
        }
    }

    pub fn in_default_state_dir(definition: &ProjectDefinition) -> Self {
        let identity = manifest_identity(&definition.manifest_path, &definition.manifest.id);
        let state_dir = crate::config::state_dir();
        Self::new(
            state_dir.join("project-tasks").join(&identity),
            state_dir.join("task-workspaces").join(identity),
        )
    }

    pub fn workspace_store_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn state_path(&self, task_id: &str) -> Result<PathBuf, ProjectError> {
        validate_identifier("task id", task_id)?;
        Ok(self.state_root.join(format!("{task_id}.json")))
    }

    pub fn lock_task_operations(
        &self,
        task_id: &str,
    ) -> Result<TaskWorkspaceOperationLock, ProjectError> {
        self.open_task_operation_lock(task_id, false)
    }

    pub fn try_lock_task_operations(
        &self,
        task_id: &str,
    ) -> Result<TaskWorkspaceOperationLock, ProjectError> {
        self.open_task_operation_lock(task_id, true)
    }

    pub fn lock_repository_operations(
        &self,
        repository_id: &str,
    ) -> Result<TaskWorkspaceOperationLock, ProjectError> {
        self.open_repository_operation_lock(repository_id, false)
    }

    pub fn try_lock_repository_operations(
        &self,
        repository_id: &str,
    ) -> Result<TaskWorkspaceOperationLock, ProjectError> {
        self.open_repository_operation_lock(repository_id, true)
    }

    fn open_repository_operation_lock(
        &self,
        repository_id: &str,
        nonblocking: bool,
    ) -> Result<TaskWorkspaceOperationLock, ProjectError> {
        validate_identifier("repository id", repository_id)?;
        self.ensure_state_root()?;
        self.open_operation_lock(
            self.state_root.join(format!(
                "{REPOSITORY_LOCK_PREFIX}{repository_id}{OPERATION_LOCK_SUFFIX}"
            )),
            format!("repository '{repository_id}' is already being changed"),
            nonblocking,
        )
    }

    pub fn create(&self, task: &TaskWorkspace) -> Result<(), ProjectError> {
        task.validate_integrity()?;
        self.validate_task_binding(task)?;
        if task.phase != TaskWorkspacePhase::Planned
            || task.revision != 0
            || !task.journal.is_empty()
        {
            return Err(ProjectError::new(
                "invalid_new_task_workspace_state",
                "new task state must be planned with revision zero and an empty journal",
            ));
        }
        self.ensure_state_root()?;
        let _store_lock = self.lock_store()?;
        self.validate_existing_state_root()?;
        let path = self.state_path(&task.id)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => return self.accept_matching_create_retry(task),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(task_io_error("destination metadata", &error)),
        }
        let bytes = serialize_task(task)?;
        match atomic_owner_only_write(&path, &bytes, PrivateWriteMode::CreateNew) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.accept_matching_create_retry(task)
            }
            Err(error) => Err(task_io_error("create", &error)),
        }
    }

    pub fn load(&self, task_id: &str) -> Result<TaskWorkspace, ProjectError> {
        let path = self.state_path(task_id)?;
        self.validate_existing_state_root()?;
        let bytes = read_bounded_regular_file(&path, MAX_TASK_STATE_BYTES)?.ok_or_else(|| {
            ProjectError::new(
                "task_workspace_not_found",
                format!("task workspace '{task_id}' does not exist"),
            )
        })?;
        let task = serde_json::from_slice::<TaskWorkspace>(&bytes).map_err(|_| {
            ProjectError::new(
                "invalid_task_workspace_state",
                "task workspace state is invalid JSON",
            )
        })?;
        if task.id != task_id {
            return Err(ProjectError::new(
                "task_workspace_identity_mismatch",
                "task workspace state belongs to a different task",
            ));
        }
        task.validate_integrity()?;
        self.validate_task_binding(&task)?;
        restrict_file_permissions(&path)?;
        Ok(task)
    }

    /// Replaces one state revision using optimistic concurrency. Every durable
    /// transition must advance by exactly one revision, preventing callers from
    /// skipping the journal's planned/applied persistence boundary.
    pub fn save(&self, task: &TaskWorkspace, expected_revision: u64) -> Result<(), ProjectError> {
        task.validate_integrity()?;
        self.validate_task_binding(task)?;
        self.validate_existing_state_root()?;
        let _store_lock = self.lock_store()?;
        let current = self.load(&task.id)?;
        if current == *task {
            return Ok(());
        }
        if current.revision != expected_revision {
            return Err(ProjectError::new(
                "task_workspace_revision_conflict",
                format!(
                    "task workspace revision changed from {expected_revision} to {}",
                    current.revision
                ),
            ));
        }
        let next_revision = current.revision.checked_add(1).ok_or_else(|| {
            ProjectError::new(
                "task_workspace_revision_exhausted",
                "task workspace revision counter is exhausted",
            )
        })?;
        if task.revision != next_revision {
            return Err(ProjectError::new(
                "invalid_task_workspace_revision",
                format!(
                    "task workspace must persist revision {next_revision} after revision {}",
                    current.revision
                ),
            ));
        }
        let bytes = serialize_task(task)?;
        let path = self.state_path(&task.id)?;
        ensure_regular_destination(&path)?;
        atomic_owner_only_write(&path, &bytes, PrivateWriteMode::Replace)
            .map_err(|error| task_io_error("save", &error))
    }

    pub fn list_ids(&self) -> Result<Vec<String>, ProjectError> {
        match fs::symlink_metadata(&self.state_root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(task_io_error("directory metadata", &error)),
            Ok(_) => self.validate_existing_state_root()?,
        }
        let mut task_ids = Vec::new();
        let mut entry_count = 0_usize;
        for entry in fs::read_dir(&self.state_root)
            .map_err(|error| task_io_error("directory read", &error))?
        {
            let entry = entry.map_err(|error| task_io_error("directory entry read", &error))?;
            entry_count = entry_count.checked_add(1).ok_or_else(|| {
                ProjectError::new(
                    "too_many_task_workspaces",
                    "project task state directory entry count is exhausted",
                )
            })?;
            if entry_count > MAX_STATE_DIRECTORY_ENTRIES {
                return Err(ProjectError::new(
                    "too_many_task_workspaces",
                    "project task state exceeds the 20000-entry safety limit",
                ));
            }
            if entry.file_name() == STORE_MARKER_FILE {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| task_io_error("task metadata", &error))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ProjectError::new(
                    "invalid_task_workspace_state",
                    "task state directory contains a non-regular entry",
                ));
            }
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                ProjectError::new(
                    "invalid_task_workspace_state",
                    "task state filename is not valid UTF-8",
                )
            })?;
            if is_private_writer_temp_name(name) {
                continue;
            }
            if operation_lock_task_id(name).is_some() {
                continue;
            }
            if operation_lock_repository_id(name).is_some() {
                continue;
            }
            let task_id = name.strip_suffix(".json").ok_or_else(|| {
                ProjectError::new(
                    "invalid_task_workspace_state",
                    "task state directory contains an unknown file",
                )
            })?;
            validate_identifier("task id", task_id)?;
            if task_ids.len() >= MAX_TASKS_PER_PROJECT {
                return Err(ProjectError::new(
                    "too_many_task_workspaces",
                    "project task state exceeds the 10000-task safety limit",
                ));
            }
            task_ids.push(task_id.to_string());
        }
        task_ids.sort();
        Ok(task_ids)
    }

    fn open_task_operation_lock(
        &self,
        task_id: &str,
        nonblocking: bool,
    ) -> Result<TaskWorkspaceOperationLock, ProjectError> {
        validate_identifier("task id", task_id)?;
        self.ensure_state_root()?;
        self.load(task_id)?;
        self.open_operation_lock(
            self.state_root.join(format!(
                "{OPERATION_LOCK_PREFIX}{task_id}{OPERATION_LOCK_SUFFIX}"
            )),
            format!("task workspace '{task_id}' is already being changed"),
            nonblocking,
        )
    }

    fn open_operation_lock(
        &self,
        path: PathBuf,
        busy_message: String,
        nonblocking: bool,
    ) -> Result<TaskWorkspaceOperationLock, ProjectError> {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(ProjectError::new(
                    "invalid_task_workspace_operation_lock",
                    "task operation lock is not a regular owner-only file",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(task_io_error("operation lock metadata", &error)),
        }
        let file = open_owner_only_lock_file(&path)
            .map_err(|error| task_io_error("operation lock open", &error))?;
        if nonblocking {
            match file.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => {
                    return Err(ProjectError::new("task_workspace_busy", busy_message));
                }
                Err(fs::TryLockError::Error(error)) => {
                    return Err(task_io_error("operation lock acquisition", &error));
                }
            }
        } else {
            file.lock()
                .map_err(|error| task_io_error("operation lock acquisition", &error))?;
        }
        restrict_file_permissions(&path)?;
        Ok(TaskWorkspaceOperationLock { _file: file })
    }

    fn accept_matching_create_retry(&self, task: &TaskWorkspace) -> Result<(), ProjectError> {
        match self.load(&task.id) {
            Ok(current) if current == *task => Ok(()),
            Ok(_) => Err(ProjectError::new(
                "task_workspace_already_exists",
                "task workspace state already exists with different contents",
            )),
            Err(error) => Err(error),
        }
    }

    fn lock_store(&self) -> Result<fs::File, ProjectError> {
        let marker = self.state_root.join(STORE_MARKER_FILE);
        let file =
            fs::File::open(marker).map_err(|error| task_io_error("store lock open", &error))?;
        file.lock()
            .map_err(|error| task_io_error("store lock acquisition", &error))?;
        Ok(file)
    }

    fn validate_task_binding(&self, task: &TaskWorkspace) -> Result<(), ProjectError> {
        validate_absolute_clean_path("task workspace store root", &self.workspace_root)?;
        if self.state_root.starts_with(&self.workspace_root)
            || self.workspace_root.starts_with(&self.state_root)
        {
            return Err(ProjectError::new(
                "task_workspace_store_overlap",
                "task state and task workspace roots must not contain one another",
            ));
        }
        validate_existing_ancestors(&self.workspace_root)?;
        if task.root != self.workspace_root.join(&task.runtime.namespace) {
            return Err(ProjectError::new(
                "task_workspace_store_mismatch",
                "task workspace root is outside its repository's data-plane root",
            ));
        }
        Ok(())
    }

    fn ensure_state_root(&self) -> Result<(), ProjectError> {
        validate_absolute_clean_path("task state store root", &self.state_root)?;
        let created = ensure_private_directory_chain(&self.state_root)?;
        let marker = self.state_root.join(STORE_MARKER_FILE);
        match read_bounded_regular_file(&marker, 128)? {
            Some(contents) if contents == STORE_MARKER_CONTENT => {
                restrict_directory_permissions(&self.state_root)?;
                restrict_file_permissions(&marker)
            }
            Some(_) => Err(ProjectError::new(
                "invalid_task_workspace_state_directory",
                "task state ownership marker is invalid",
            )),
            None if recover_interrupted_marker_create(&self.state_root, &marker)? => Ok(()),
            None if created || directory_is_empty(&self.state_root)? => {
                match atomic_owner_only_write(
                    &marker,
                    STORE_MARKER_CONTENT,
                    PrivateWriteMode::CreateNew,
                ) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        self.validate_existing_state_root()
                    }
                    Err(error) => Err(task_io_error("ownership marker creation", &error)),
                }
            }
            None => Err(ProjectError::new(
                "invalid_task_workspace_state_directory",
                "refusing to adopt a non-empty directory without a GoWild ownership marker",
            )),
        }
    }

    fn validate_existing_state_root(&self) -> Result<(), ProjectError> {
        validate_absolute_clean_path("task state store root", &self.state_root)?;
        validate_existing_directory_chain(&self.state_root)?;
        let marker = self.state_root.join(STORE_MARKER_FILE);
        let contents = read_bounded_regular_file(&marker, 128)?.ok_or_else(|| {
            ProjectError::new(
                "invalid_task_workspace_state_directory",
                "task state directory has no GoWild ownership marker",
            )
        })?;
        if contents != STORE_MARKER_CONTENT {
            return Err(ProjectError::new(
                "invalid_task_workspace_state_directory",
                "task state ownership marker is invalid",
            ));
        }
        restrict_directory_permissions(&self.state_root)?;
        restrict_file_permissions(&marker)
    }
}
