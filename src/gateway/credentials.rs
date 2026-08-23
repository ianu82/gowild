use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use keyring::Entry;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const KEYRING_SERVICE: &str = "io.mindshub.gowild.gateway";
const CREDENTIAL_FILE_SCHEMA_VERSION: u32 = 1;
const MAX_CREDENTIAL_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// A secret value whose debug representation is always redacted and which is
/// zeroed when dropped. It intentionally does not implement Serialize.
#[derive(Clone)]
pub(crate) struct Credential {
    value: Zeroizing<String>,
}

impl Credential {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, CredentialStoreError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 16_384 || value.chars().any(char::is_control) {
            return Err(CredentialStoreError::InvalidCredential);
        }
        Ok(Self {
            value: Zeroizing::new(value),
        })
    }

    fn from_zeroizing(value: Zeroizing<String>) -> Result<Self, CredentialStoreError> {
        if value.trim().is_empty() || value.len() > 16_384 || value.chars().any(char::is_control) {
            return Err(CredentialStoreError::InvalidCredential);
        }
        Ok(Self { value })
    }

    pub(crate) fn expose(&self) -> &str {
        self.value.as_str()
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Credential([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialBackend {
    System,
    RestrictedFile,
}

pub(crate) trait CredentialStore {
    fn get(&self, credential_ref: &str) -> Result<Option<Credential>, CredentialStoreError>;
    fn set(
        &self,
        credential_ref: &str,
        credential: &Credential,
    ) -> Result<CredentialBackend, CredentialStoreError>;
    fn delete(&self, credential_ref: &str) -> Result<(), CredentialStoreError>;
}

#[derive(Debug)]
pub(crate) enum CredentialStoreError {
    InvalidReference,
    InvalidCredential,
    SystemStoreUnavailable,
    FileFallbackUnsupported,
    Io {
        operation: &'static str,
        kind: io::ErrorKind,
    },
    CorruptFile,
    UnsupportedFileVersion(u32),
}

impl fmt::Display for CredentialStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReference => formatter.write_str("invalid credential reference"),
            Self::InvalidCredential => formatter.write_str("credential is empty or invalid"),
            Self::SystemStoreUnavailable => {
                formatter.write_str("the operating system credential store is unavailable")
            }
            Self::FileFallbackUnsupported => formatter
                .write_str("a secure file credential fallback is not supported on this platform"),
            Self::Io { operation, kind } => {
                write!(formatter, "credential file {operation} failed ({kind:?})")
            }
            Self::CorruptFile => formatter.write_str("credential file is invalid"),
            Self::UnsupportedFileVersion(version) => write!(
                formatter,
                "credential file schema version {version} is not supported"
            ),
        }
    }
}

impl std::error::Error for CredentialStoreError {}

/// Uses Keychain Services, Windows Credential Manager, or Secret Service.
/// Unix systems can fall back to a separate owner-only file when the platform
/// store is unavailable; Windows fails closed instead of writing a file whose
/// ACL GoWild cannot prove is private.
pub(crate) struct SystemCredentialStore {
    fallback: Option<FileCredentialStore>,
}

impl SystemCredentialStore {
    pub(crate) fn new(config_dir: &Path) -> Self {
        #[cfg(unix)]
        let fallback = Some(FileCredentialStore::new(
            config_dir.join("credentials.json"),
        ));
        #[cfg(not(unix))]
        let fallback = {
            let _ = config_dir;
            None
        };
        Self { fallback }
    }

    fn entry(credential_ref: &str) -> Result<Entry, CredentialStoreError> {
        validate_reference(credential_ref)?;
        Entry::new(KEYRING_SERVICE, credential_ref)
            .map_err(|_| CredentialStoreError::SystemStoreUnavailable)
    }
}

impl CredentialStore for SystemCredentialStore {
    fn get(&self, credential_ref: &str) -> Result<Option<Credential>, CredentialStoreError> {
        validate_reference(credential_ref)?;
        let entry = match Self::entry(credential_ref) {
            Ok(entry) => entry,
            Err(_) => return self.fallback_get_for(credential_ref),
        };
        match entry.get_password() {
            Ok(value) => Credential::new(value).map(Some),
            Err(keyring::Error::NoEntry) => self
                .fallback
                .as_ref()
                .map_or(Ok(None), |fallback| fallback.get(credential_ref)),
            Err(_) => self.fallback_get_for(credential_ref),
        }
    }

    fn set(
        &self,
        credential_ref: &str,
        credential: &Credential,
    ) -> Result<CredentialBackend, CredentialStoreError> {
        validate_reference(credential_ref)?;
        let keyring_result = Self::entry(credential_ref).and_then(|entry| {
            entry
                .set_password(credential.expose())
                .map_err(|_| CredentialStoreError::SystemStoreUnavailable)
        });
        if keyring_result.is_ok() {
            if let Some(fallback) = &self.fallback {
                fallback.delete(credential_ref)?;
            }
            return Ok(CredentialBackend::System);
        }

        let Some(fallback) = &self.fallback else {
            return Err(CredentialStoreError::SystemStoreUnavailable);
        };
        fallback.set(credential_ref, credential)?;
        Ok(CredentialBackend::RestrictedFile)
    }

    fn delete(&self, credential_ref: &str) -> Result<(), CredentialStoreError> {
        validate_reference(credential_ref)?;
        let keyring_result = match Self::entry(credential_ref) {
            Ok(entry) => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(CredentialStoreError::SystemStoreUnavailable),
            },
            Err(error) => Err(error),
        };
        let fallback_result = self
            .fallback
            .as_ref()
            .map_or(Ok(()), |fallback| fallback.delete(credential_ref));
        keyring_result.and(fallback_result)
    }
}

impl SystemCredentialStore {
    fn fallback_get_for(
        &self,
        credential_ref: &str,
    ) -> Result<Option<Credential>, CredentialStoreError> {
        let Some(fallback) = &self.fallback else {
            return Err(CredentialStoreError::SystemStoreUnavailable);
        };
        fallback
            .get(credential_ref)?
            .map(Some)
            .ok_or(CredentialStoreError::SystemStoreUnavailable)
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    schema_version: u32,
    #[serde(default)]
    credentials: BTreeMap<String, Zeroizing<String>>,
}

/// Owner-only Unix fallback, kept separate from shareable gateway metadata.
pub(crate) struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> Result<CredentialFile, CredentialStoreError> {
        ensure_file_fallback_supported()?;
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(CredentialFile {
                    schema_version: CREDENTIAL_FILE_SCHEMA_VERSION,
                    ..CredentialFile::default()
                });
            }
            Err(error) => return Err(io_error("metadata", &error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CredentialStoreError::CorruptFile);
        }
        if metadata.len() > MAX_CREDENTIAL_FILE_BYTES {
            return Err(CredentialStoreError::CorruptFile);
        }
        restrict_file_permissions(&self.path)?;
        let data = fs::read(&self.path).map_err(|error| io_error("read", &error))?;
        let credentials: CredentialFile =
            serde_json::from_slice(&data).map_err(|_| CredentialStoreError::CorruptFile)?;
        if credentials.schema_version != CREDENTIAL_FILE_SCHEMA_VERSION {
            return Err(CredentialStoreError::UnsupportedFileVersion(
                credentials.schema_version,
            ));
        }
        Ok(credentials)
    }

    fn save(&self, credentials: &CredentialFile) -> Result<(), CredentialStoreError> {
        ensure_file_fallback_supported()?;
        let parent = self
            .path
            .parent()
            .ok_or(CredentialStoreError::CorruptFile)?;
        fs::create_dir_all(parent).map_err(|error| io_error("directory creation", &error))?;
        restrict_directory_permissions(parent)?;

        let bytes = Zeroizing::new(
            serde_json::to_vec_pretty(credentials)
                .map_err(|_| CredentialStoreError::CorruptFile)?,
        );
        if bytes.len() as u64 > MAX_CREDENTIAL_FILE_BYTES {
            return Err(CredentialStoreError::CorruptFile);
        }
        let temp_path = unique_temp_path(&self.path);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temp_path)
            .map_err(|error| io_error("temporary-file creation", &error))?;
        let write_result = file
            .write_all(bytes.as_slice())
            .and_then(|()| file.sync_all());
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temp_path);
            return Err(io_error("write", &error));
        }
        drop(file);
        if let Err(error) = restrict_file_permissions(&temp_path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temp_path, &self.path) {
            let _ = fs::remove_file(&temp_path);
            return Err(io_error("commit", &error));
        }
        Ok(())
    }
}

impl CredentialStore for FileCredentialStore {
    fn get(&self, credential_ref: &str) -> Result<Option<Credential>, CredentialStoreError> {
        validate_reference(credential_ref)?;
        let mut file = self.load()?;
        let Some(value) = file.credentials.remove(credential_ref) else {
            return Ok(None);
        };
        Credential::from_zeroizing(value).map(Some)
    }

    fn set(
        &self,
        credential_ref: &str,
        credential: &Credential,
    ) -> Result<CredentialBackend, CredentialStoreError> {
        validate_reference(credential_ref)?;
        let mut file = self.load()?;
        file.credentials.insert(
            credential_ref.into(),
            Zeroizing::new(credential.expose().to_string()),
        );
        self.save(&file)?;
        Ok(CredentialBackend::RestrictedFile)
    }

    fn delete(&self, credential_ref: &str) -> Result<(), CredentialStoreError> {
        validate_reference(credential_ref)?;
        let mut file = self.load()?;
        if file.credentials.remove(credential_ref).is_some() {
            self.save(&file)?;
        }
        Ok(())
    }
}

fn validate_reference(credential_ref: &str) -> Result<(), CredentialStoreError> {
    if credential_ref.trim().is_empty()
        || credential_ref.len() > 128
        || credential_ref.chars().any(char::is_control)
    {
        Err(CredentialStoreError::InvalidReference)
    } else {
        Ok(())
    }
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("credentials");
    path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), nonce))
}

fn io_error(operation: &'static str, error: &io::Error) -> CredentialStoreError {
    CredentialStoreError::Io {
        operation,
        kind: error.kind(),
    }
}

#[cfg(unix)]
fn ensure_file_fallback_supported() -> Result<(), CredentialStoreError> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_file_fallback_supported() -> Result<(), CredentialStoreError> {
    Err(CredentialStoreError::FileFallbackUnsupported)
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> Result<(), CredentialStoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| io_error("permission update", &error))
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> Result<(), CredentialStoreError> {
    Err(CredentialStoreError::FileFallbackUnsupported)
}

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) -> Result<(), CredentialStoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error("directory permission update", &error))
}

#[cfg(not(unix))]
fn restrict_directory_permissions(_path: &Path) -> Result<(), CredentialStoreError> {
    Err(CredentialStoreError::FileFallbackUnsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_debug_is_always_redacted() {
        let credential = Credential::new("mdb_very-secret-value").unwrap();
        let debug = format!("{credential:?}");
        assert_eq!(debug, "Credential([REDACTED])");
        assert!(!debug.contains("very-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn file_store_round_trips_separately_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "gowild-credential-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("credentials.json");
        let store = FileCredentialStore::new(path.clone());
        let credential = Credential::new("mdb_test-secret-value").unwrap();

        assert_eq!(
            store.set("gateway:test", &credential).unwrap(),
            CredentialBackend::RestrictedFile
        );
        assert_eq!(
            store.get("gateway:test").unwrap().unwrap().expose(),
            credential.expose()
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );

        store.delete("gateway:test").unwrap();
        assert!(store.get("gateway:test").unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn file_store_refuses_a_symlinked_credential_file() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "gowild-credential-symlink-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target.json");
        fs::write(&target, "do not overwrite").unwrap();
        let path = root.join("credentials.json");
        symlink(&target, &path).unwrap();
        let store = FileCredentialStore::new(path);
        let credential = Credential::new("mdb_test-secret-value").unwrap();
        assert!(matches!(
            store.set("gateway:test", &credential),
            Err(CredentialStoreError::CorruptFile)
        ));
        assert_eq!(fs::read_to_string(&target).unwrap(), "do not overwrite");
        fs::remove_dir_all(root).unwrap();
    }
}
