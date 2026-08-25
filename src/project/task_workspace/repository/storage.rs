use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use super::{
    MAX_TASK_STATE_BYTES, OPERATION_LOCK_PREFIX, OPERATION_LOCK_SUFFIX, REPOSITORY_LOCK_PREFIX,
    STORE_MARKER_CONTENT,
};
use crate::project::private_state::write::{atomic_owner_only_write, PrivateWriteMode};
use crate::project::task_workspace::rules::validate_identifier;
use crate::project::task_workspace::TaskWorkspace;
use crate::project::ProjectError;

pub(super) fn serialize_task(task: &TaskWorkspace) -> Result<Vec<u8>, ProjectError> {
    let bytes = serde_json::to_vec_pretty(task).map_err(|_| {
        ProjectError::new(
            "invalid_task_workspace_state",
            "could not serialize task workspace state",
        )
    })?;
    if bytes.len() as u64 > MAX_TASK_STATE_BYTES {
        return Err(ProjectError::new(
            "task_workspace_state_too_large",
            "task workspace state exceeds 16 MiB",
        ));
    }
    Ok(bytes)
}

pub(super) fn ensure_regular_destination(path: &Path) -> Result<(), ProjectError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| task_io_error("destination metadata", &error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        Err(ProjectError::new(
            "invalid_task_workspace_state",
            "task workspace state destination is not a regular file",
        ))
    } else {
        Ok(())
    }
}

pub(super) fn recover_interrupted_marker_create(
    state_root: &Path,
    marker: &Path,
) -> Result<bool, ProjectError> {
    let mut candidate = None;
    let mut entry_count = 0_usize;
    for entry in
        fs::read_dir(state_root).map_err(|error| task_io_error("directory read", &error))?
    {
        let entry = entry.map_err(|error| task_io_error("directory entry read", &error))?;
        entry_count += 1;
        if entry_count > 16 {
            return Ok(false);
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Ok(false);
        };
        if !is_private_writer_temp_name(name) {
            return Ok(false);
        }
        let contents = read_bounded_regular_file(&entry.path(), 128)?;
        if contents.as_deref() == Some(STORE_MARKER_CONTENT) && candidate.is_none() {
            candidate = Some(entry.path());
        }
    }
    if entry_count == 0 {
        return Ok(false);
    }
    let Some(candidate) = candidate else {
        return match atomic_owner_only_write(
            marker,
            STORE_MARKER_CONTENT,
            PrivateWriteMode::CreateNew,
        ) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let contents = read_bounded_regular_file(marker, 128)?;
                Ok(contents.as_deref() == Some(STORE_MARKER_CONTENT))
            }
            Err(error) => Err(task_io_error("ownership marker recovery", &error)),
        };
    };
    match fs::hard_link(&candidate, marker) {
        Ok(()) => {
            let _ = fs::remove_file(candidate);
            restrict_file_permissions(marker)?;
            sync_directory(state_root)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let contents = read_bounded_regular_file(marker, 128)?;
            Ok(contents.as_deref() == Some(STORE_MARKER_CONTENT))
        }
        Err(error) => Err(task_io_error("ownership marker recovery", &error)),
    }
}

pub(super) fn is_private_writer_temp_name(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix(".project-state.")
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let mut parts = body.split('.');
    let valid_part =
        |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    valid_part(parts.next().unwrap_or_default())
        && valid_part(parts.next().unwrap_or_default())
        && valid_part(parts.next().unwrap_or_default())
        && parts.next().is_none()
}

pub(super) fn operation_lock_task_id(name: &str) -> Option<&str> {
    let task_id = name
        .strip_prefix(OPERATION_LOCK_PREFIX)?
        .strip_suffix(OPERATION_LOCK_SUFFIX)?;
    validate_identifier("task id", task_id)
        .is_ok()
        .then_some(task_id)
}

pub(super) fn operation_lock_repository_id(name: &str) -> Option<&str> {
    let repository_id = name
        .strip_prefix(REPOSITORY_LOCK_PREFIX)?
        .strip_suffix(OPERATION_LOCK_SUFFIX)?;
    validate_identifier("repository id", repository_id)
        .is_ok()
        .then_some(repository_id)
}

pub(super) fn open_owner_only_lock_file(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

pub(super) fn read_bounded_regular_file(
    path: &Path,
    max_bytes: u64,
) -> Result<Option<Vec<u8>>, ProjectError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(task_io_error("file metadata", &error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(ProjectError::new(
            "invalid_task_workspace_state",
            "task workspace state must be a bounded regular file, not a symlink",
        ));
    }
    let file = fs::File::open(path).map_err(|error| task_io_error("file open", &error))?;
    let mut bytes = Vec::with_capacity(metadata.len().min(max_bytes) as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| task_io_error("file read", &error))?;
    if bytes.len() as u64 > max_bytes {
        return Err(ProjectError::new(
            "invalid_task_workspace_state",
            "task workspace state exceeds its size limit",
        ));
    }
    Ok(Some(bytes))
}

pub(in crate::project::task_workspace) fn ensure_private_directory_chain(
    path: &Path,
) -> Result<bool, ProjectError> {
    if path.parent().is_none() {
        return Err(ProjectError::new(
            "invalid_task_workspace_state_directory",
            "task state directory cannot be the filesystem root",
        ));
    }
    let mut current = PathBuf::new();
    let mut final_created = false;
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(ProjectError::new(
                    "invalid_task_workspace_state_directory",
                    "task state directory path contains a symlink or non-directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match crate::platform::create_private_directory(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        let metadata = fs::symlink_metadata(&current)
                            .map_err(|error| task_io_error("directory metadata", &error))?;
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            return Err(ProjectError::new(
                                "invalid_task_workspace_state_directory",
                                "task state directory path contains a symlink or non-directory",
                            ));
                        }
                    }
                    Err(error) => return Err(task_io_error("directory creation", &error)),
                }
                restrict_directory_permissions(&current)?;
                final_created = current == path;
            }
            Err(error) => return Err(task_io_error("directory metadata", &error)),
        }
    }
    Ok(final_created)
}

pub(super) fn validate_existing_directory_chain(path: &Path) -> Result<(), ProjectError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| task_io_error("directory metadata", &error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ProjectError::new(
                "invalid_task_workspace_state_directory",
                "task state directory path contains a symlink or non-directory",
            ));
        }
    }
    Ok(())
}

pub(in crate::project::task_workspace) fn validate_existing_ancestors(
    path: &Path,
) -> Result<(), ProjectError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(ProjectError::new(
                    "invalid_task_workspace_store",
                    "task workspace store path contains a symlink or non-directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(task_io_error("workspace ancestor metadata", &error)),
        }
    }
    Ok(())
}

pub(in crate::project::task_workspace) fn directory_is_empty(
    path: &Path,
) -> Result<bool, ProjectError> {
    Ok(fs::read_dir(path)
        .map_err(|error| task_io_error("directory read", &error))?
        .next()
        .is_none())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), ProjectError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| task_io_error("directory sync", &error))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), ProjectError> {
    Ok(())
}

#[cfg(unix)]
pub(in crate::project::task_workspace) fn restrict_directory_permissions(
    path: &Path,
) -> Result<(), ProjectError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| task_io_error("directory permission update", &error))
}

#[cfg(not(unix))]
pub(in crate::project::task_workspace) fn restrict_directory_permissions(
    _path: &Path,
) -> Result<(), ProjectError> {
    Ok(())
}

#[cfg(unix)]
pub(super) fn restrict_file_permissions(path: &Path) -> Result<(), ProjectError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| task_io_error("file permission update", &error))
}

#[cfg(not(unix))]
pub(super) fn restrict_file_permissions(_path: &Path) -> Result<(), ProjectError> {
    Ok(())
}

pub(super) fn task_io_error(operation: &'static str, error: &io::Error) -> ProjectError {
    ProjectError::new(
        "task_workspace_state_io",
        format!(
            "task workspace state {operation} failed ({:?})",
            error.kind()
        ),
    )
}
