use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::model::{GatewayCatalog, ValidationError};

const MAX_GATEWAY_FILE_BYTES: u64 = 5 * 1024 * 1024;

pub(crate) struct GatewayRepository {
    path: PathBuf,
}

impl GatewayRepository {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn in_default_config_dir() -> Self {
        Self::new(crate::config::config_dir().join("gateways.json"))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn load(&self) -> Result<GatewayCatalog, GatewayRepositoryError> {
        match fs::metadata(&self.path) {
            Ok(metadata) if metadata.len() > MAX_GATEWAY_FILE_BYTES => {
                return Err(GatewayRepositoryError::InvalidFile);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(GatewayCatalog::with_builtin_presets());
            }
            Err(error) => return Err(GatewayRepositoryError::io("metadata", &error)),
        }
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) => return Err(GatewayRepositoryError::io("read", &error)),
        };
        if bytes.len() as u64 > MAX_GATEWAY_FILE_BYTES {
            return Err(GatewayRepositoryError::InvalidFile);
        }
        let catalog: GatewayCatalog =
            serde_json::from_slice(&bytes).map_err(|_| GatewayRepositoryError::InvalidFile)?;
        let errors = catalog.validate();
        if errors.is_empty() {
            Ok(catalog)
        } else {
            Err(GatewayRepositoryError::Validation(errors))
        }
    }

    pub(crate) fn save(&self, catalog: &GatewayCatalog) -> Result<(), GatewayRepositoryError> {
        let errors = catalog.validate();
        if !errors.is_empty() {
            return Err(GatewayRepositoryError::Validation(errors));
        }
        let bytes =
            serde_json::to_vec_pretty(catalog).map_err(|_| GatewayRepositoryError::InvalidFile)?;
        if bytes.len() as u64 > MAX_GATEWAY_FILE_BYTES {
            return Err(GatewayRepositoryError::InvalidFile);
        }
        atomic_write(&self.path, &bytes)
    }
}

#[derive(Debug)]
pub(crate) enum GatewayRepositoryError {
    Io {
        operation: &'static str,
        kind: io::ErrorKind,
    },
    InvalidFile,
    Validation(Vec<ValidationError>),
}

impl GatewayRepositoryError {
    fn io(operation: &'static str, error: &io::Error) -> Self {
        Self::Io {
            operation,
            kind: error.kind(),
        }
    }
}

impl fmt::Display for GatewayRepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, kind } => {
                write!(
                    formatter,
                    "gateway configuration {operation} failed ({kind:?})"
                )
            }
            Self::InvalidFile => formatter.write_str("gateway configuration is invalid JSON"),
            Self::Validation(errors) => {
                write!(formatter, "gateway configuration is invalid")?;
                for error in errors {
                    write!(formatter, "; {error}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for GatewayRepositoryError {}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), GatewayRepositoryError> {
    let parent = path.parent().ok_or(GatewayRepositoryError::InvalidFile)?;
    fs::create_dir_all(parent)
        .map_err(|error| GatewayRepositoryError::io("directory creation", &error))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gateways");
    let temp_path = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), nonce));

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp_path)
        .map_err(|error| GatewayRepositoryError::io("temporary-file creation", &error))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp_path);
        return Err(GatewayRepositoryError::io("write", &error));
    }
    drop(file);

    if let Err(error) = replace_file(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(GatewayRepositoryError::io("commit", &error));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_WRITE_THROUGH};

    if !destination.exists() {
        return fs::rename(source, destination);
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::credentials::{Credential, CredentialStore, FileCredentialStore};

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gowild-{name}-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn missing_file_loads_the_builtin_mindshub_preset() {
        let path = test_path("missing-gateways");
        let repository = GatewayRepository::new(path);
        let catalog = repository.load().unwrap();
        assert!(catalog.gateways.contains_key("mindshub"));
        assert!(catalog.default_gateway_id.is_none());
    }

    #[test]
    fn valid_catalog_round_trips_without_secret_fields() {
        let path = test_path("gateway-roundtrip");
        let repository = GatewayRepository::new(path.clone());
        let mut catalog = GatewayCatalog::with_builtin_presets();
        catalog.default_gateway_id = Some("mindshub".into());
        repository.save(&catalog).unwrap();

        let json = fs::read_to_string(&path).unwrap();
        assert!(!json.to_ascii_lowercase().contains("api_key"));
        assert!(!json.contains("mdb_"));
        assert_eq!(repository.load().unwrap(), catalog);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn raw_secret_fields_are_rejected_in_non_secret_config() {
        let path = test_path("gateway-secret-field");
        let repository = GatewayRepository::new(path.clone());
        let json = serde_json::to_string(&GatewayCatalog::with_builtin_presets()).unwrap();
        let injected = json.replacen(
            "\"display_name\":\"MindsHub Inference\"",
            "\"display_name\":\"MindsHub Inference\",\"api_key\":\"mdb_should-never-load\"",
            1,
        );
        fs::write(&path, injected).unwrap();
        assert!(matches!(
            repository.load(),
            Err(GatewayRepositoryError::InvalidFile)
        ));
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn credentials_remain_separate_from_gateway_metadata() {
        let root = test_path("separate-secret-root").with_extension("");
        fs::create_dir_all(&root).unwrap();
        let repository = GatewayRepository::new(root.join("gateways.json"));
        let store = FileCredentialStore::new(root.join("credentials.json"));
        let credential = Credential::new("mdb_a-real-secret-value").unwrap();

        repository
            .save(&GatewayCatalog::with_builtin_presets())
            .unwrap();
        store.set("gateway:mindshub", &credential).unwrap();

        let metadata = fs::read_to_string(repository.path()).unwrap();
        let secrets = fs::read_to_string(root.join("credentials.json")).unwrap();
        assert!(!metadata.contains(credential.expose()));
        assert!(secrets.contains(credential.expose()));
        fs::remove_dir_all(root).unwrap();
    }
}
