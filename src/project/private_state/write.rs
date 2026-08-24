use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::super::manifest::ProjectDefinition;
use super::super::model::ProjectError;
use super::{
    overrides_digest, private_io_error, restrict_file_permissions, ProjectPrivateState,
    ProjectPrivateStateRepository, ProjectTrust, MAX_PRIVATE_STATE_BYTES,
};

static NEXT_PRIVATE_WRITE: AtomicU64 = AtomicU64::new(0);

impl ProjectPrivateState {
    pub fn grant_trust(
        &mut self,
        definition: &ProjectDefinition,
        expected_digest: &str,
    ) -> Result<(), ProjectError> {
        if expected_digest != definition.digest {
            return Err(ProjectError::new(
                "project_trust_digest_mismatch",
                "the supplied digest does not match the current project manifest",
            ));
        }
        self.overrides.validate_for(&definition.manifest)?;
        self.trust = Some(ProjectTrust {
            manifest_digest: definition.digest.clone(),
            overrides_digest: overrides_digest(&self.overrides),
            granted_at_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });
        Ok(())
    }

    pub fn revoke_trust(&mut self) -> bool {
        self.trust.take().is_some()
    }
}

impl ProjectPrivateStateRepository {
    pub fn save(
        &self,
        definition: &ProjectDefinition,
        state: &ProjectPrivateState,
    ) -> Result<(), ProjectError> {
        state.validate_for(definition)?;
        ensure_private_directory(&self.root)?;
        let path = self.path_for(definition);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(ProjectError::new(
                    "invalid_project_private_state",
                    "project private state destination is not a regular file",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(private_io_error("destination metadata", &error)),
        }
        let bytes = serde_json::to_vec_pretty(state).map_err(|_| {
            ProjectError::new(
                "invalid_project_private_state",
                "could not serialize project private state",
            )
        })?;
        if bytes.len() as u64 > MAX_PRIVATE_STATE_BYTES {
            return Err(ProjectError::new(
                "invalid_project_private_state",
                "project private state exceeds 1 MiB",
            ));
        }
        atomic_private_write(&path, &bytes)
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), ProjectError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(ProjectError::new(
                "invalid_project_private_state_directory",
                "project private state directory must be a regular directory, not a symlink",
            ));
        }
        Ok(_) => return restrict_directory_permissions(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(private_io_error("directory metadata", &error)),
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| private_io_error("directory creation", &error))?;
    }
    match crate::platform::create_private_directory(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            match fs::symlink_metadata(path) {
                Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {
                    restrict_directory_permissions(path)
                }
                Ok(_) => Err(ProjectError::new(
                    "invalid_project_private_state_directory",
                    "project private state directory must be a regular directory, not a symlink",
                )),
                Err(error) => Err(private_io_error("directory metadata", &error)),
            }
        }
        Err(error) => Err(private_io_error("private directory creation", &error)),
    }
}

fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    let parent = path.parent().ok_or_else(|| {
        ProjectError::new(
            "invalid_project_private_state",
            "project private state has no parent directory",
        )
    })?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_PRIVATE_WRITE.fetch_add(1, Ordering::Relaxed);
    let temp_path = parent.join(format!(
        ".project-state.{}.{}.{}.tmp",
        std::process::id(),
        nonce,
        sequence
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp_path)
        .map_err(|error| private_io_error("temporary-file creation", &error))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp_path);
        return Err(private_io_error("write", &error));
    }
    drop(file);
    if let Err(error) = replace_private_file(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(private_io_error("commit", &error));
    }
    restrict_file_permissions(path)?;
    sync_private_directory(parent)
}

#[cfg(not(windows))]
fn replace_private_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_private_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_WRITE_THROUGH};

    if !destination.exists() {
        return fs::rename(source, destination);
    }
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            source.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) -> Result<(), ProjectError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| private_io_error("directory permission update", &error))
}

#[cfg(not(unix))]
fn restrict_directory_permissions(_path: &Path) -> Result<(), ProjectError> {
    Ok(())
}

#[cfg(unix)]
fn sync_private_directory(path: &Path) -> Result<(), ProjectError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| private_io_error("directory sync", &error))
}

#[cfg(not(unix))]
fn sync_private_directory(_path: &Path) -> Result<(), ProjectError> {
    Ok(())
}
