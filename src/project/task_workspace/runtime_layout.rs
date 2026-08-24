use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::provision::{verify_owned_task_root, TaskWorkspaceProvisioner};
use super::repository::{
    directory_is_empty, ensure_private_directory_chain, restrict_directory_permissions,
    validate_existing_ancestors,
};
use super::{OwnedResource, TaskWorkspace};
use crate::project::ProjectError;

const RUNTIME_MARKER_PREFIX: &str = ".gowild-runtime-directory-v1";

impl TaskWorkspaceProvisioner<'_> {
    pub(super) fn ensure_runtime_layout(
        &self,
        task: &mut TaskWorkspace,
    ) -> Result<(), ProjectError> {
        for path in runtime_directories(task) {
            let resource = OwnedResource::RuntimeDirectory { path: path.clone() };
            let verify_task = task.clone();
            let ensure_task = task.clone();
            let verify_path = path.clone();
            self.ensure_acquired(
                task,
                resource,
                || verify_owned_runtime_directory(&verify_task, &verify_path),
                || ensure_owned_runtime_directory(&ensure_task, &path),
            )?;
        }
        Ok(())
    }
}

pub(super) fn verify_runtime_layout(task: &TaskWorkspace) -> Result<(), ProjectError> {
    for path in runtime_directories(task) {
        let resource = OwnedResource::RuntimeDirectory { path: path.clone() };
        if !task.resource_is_owned(&resource) {
            return Err(ProjectError::new(
                "task_runtime_ownership_mismatch",
                "task journal does not own its complete runtime directory layout",
            ));
        }
        verify_owned_runtime_directory(task, &path)?;
    }
    Ok(())
}

pub(super) fn runtime_directories(task: &TaskWorkspace) -> [PathBuf; 4] {
    [
        task.runtime.root.clone(),
        task.runtime.temp.clone(),
        task.runtime.cache.clone(),
        task.runtime.data.clone(),
    ]
}

pub(super) fn runtime_directory_marker(
    task: &TaskWorkspace,
    path: &Path,
) -> Result<PathBuf, ProjectError> {
    let kind = runtime_directory_kind(task, path)?;
    Ok(path.join(format!(
        "{RUNTIME_MARKER_PREFIX}-{kind}-{}-{}-{}",
        task.project_id, task.id, task.manifest_digest
    )))
}

pub(super) fn ensure_owned_runtime_directory(
    task: &TaskWorkspace,
    path: &Path,
) -> Result<(), ProjectError> {
    verify_runtime_parent(task, path)?;
    validate_existing_ancestors(path)?;
    let created = ensure_private_directory_chain(path)?;
    let marker = runtime_directory_marker(task, path)?;
    match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(ProjectError::new(
                "task_runtime_ownership_mismatch",
                "runtime directory ownership marker is not a private directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !created && !directory_is_empty(path)? {
                return Err(ProjectError::new(
                    "task_runtime_ownership_mismatch",
                    "refusing to adopt a non-empty runtime directory without its ownership marker",
                ));
            }
            crate::platform::create_private_directory(&marker)
                .map_err(|error| runtime_io_error("marker creation", &error))?;
        }
        Err(error) => return Err(runtime_io_error("marker metadata", &error)),
    }
    restrict_directory_permissions(path)?;
    restrict_directory_permissions(&marker)?;
    verify_owned_runtime_directory(task, path)
}

pub(super) fn verify_owned_runtime_directory(
    task: &TaskWorkspace,
    path: &Path,
) -> Result<(), ProjectError> {
    verify_runtime_parent(task, path)?;
    validate_existing_ancestors(path)?;
    let directory = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ProjectError::new(
                "task_runtime_directory_missing",
                "task runtime directory is missing",
            ));
        }
        Err(error) => return Err(runtime_io_error("directory metadata", &error)),
    };
    let marker = match fs::symlink_metadata(runtime_directory_marker(task, path)?) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ProjectError::new(
                "task_runtime_ownership_mismatch",
                "task runtime directory ownership marker is missing",
            ));
        }
        Err(error) => return Err(runtime_io_error("marker metadata", &error)),
    };
    if directory.file_type().is_symlink()
        || !directory.is_dir()
        || marker.file_type().is_symlink()
        || !marker.is_dir()
    {
        return Err(ProjectError::new(
            "task_runtime_ownership_mismatch",
            "task runtime directory or marker is not an owned directory",
        ));
    }
    Ok(())
}

pub(super) fn validate_releasable_runtime_directory(
    task: &TaskWorkspace,
    path: &Path,
    allow_partial: bool,
) -> Result<(), ProjectError> {
    runtime_directory_kind(task, path)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return if allow_partial {
                Ok(())
            } else {
                Err(ProjectError::new(
                    "task_workspace_cleanup_conflict",
                    "owned runtime directory is missing",
                ))
            };
        }
        Err(error) => return Err(runtime_io_error("directory metadata", &error)),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(ProjectError::new(
                "task_workspace_cleanup_conflict",
                "runtime directory is a symlink or non-directory",
            ));
        }
        Ok(_) => {}
    }
    if allow_partial {
        verify_runtime_parent(task, path)
    } else {
        verify_owned_runtime_directory(task, path)
    }
}

pub(super) fn ensure_runtime_directory_released(
    task: &TaskWorkspace,
    path: &Path,
) -> Result<(), ProjectError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(runtime_io_error("directory metadata", &error)),
        Ok(_) => {}
    }
    validate_releasable_runtime_directory(task, path, true)?;
    fs::remove_dir_all(path).map_err(|error| runtime_io_error("directory removal", &error))?;
    verify_runtime_directory_released(path)
}

pub(super) fn verify_runtime_directory_released(path: &Path) -> Result<(), ProjectError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(runtime_io_error("released directory metadata", &error)),
        Ok(_) => Err(ProjectError::new(
            "task_workspace_cleanup_conflict",
            "task runtime directory still contains data",
        )),
    }
}

fn verify_runtime_parent(task: &TaskWorkspace, path: &Path) -> Result<(), ProjectError> {
    let kind = runtime_directory_kind(task, path)?;
    if kind == "root" {
        verify_owned_task_root(task)
    } else {
        verify_owned_runtime_directory(task, &task.runtime.root)
    }
}

fn runtime_directory_kind(task: &TaskWorkspace, path: &Path) -> Result<&'static str, ProjectError> {
    if path == task.runtime.root {
        Ok("root")
    } else if path == task.runtime.temp {
        Ok("temp")
    } else if path == task.runtime.cache {
        Ok("cache")
    } else if path == task.runtime.data {
        Ok("data")
    } else {
        Err(ProjectError::new(
            "unowned_task_workspace_resource",
            "runtime directory is outside this task's ownership boundary",
        ))
    }
}

fn runtime_io_error(operation: &'static str, error: &io::Error) -> ProjectError {
    ProjectError::new(
        "task_runtime_directory_io",
        format!("task runtime {operation} failed ({:?})", error.kind()),
    )
}
