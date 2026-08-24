use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::ProjectDefinition;
use super::model::ProjectError;
use super::overrides::ProjectOverrides;

mod write;

const PRIVATE_STATE_VERSION: u32 = 1;
const MAX_PRIVATE_STATE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectTrust {
    manifest_digest: String,
    overrides_digest: String,
    granted_at_unix_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectTrustStatus {
    NotRequired,
    Trusted,
    Untrusted,
    Stale,
}

impl fmt::Display for ProjectTrustStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotRequired => "not required",
            Self::Trusted => "trusted",
            Self::Untrusted => "untrusted",
            Self::Stale => "stale",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPrivateState {
    schema_version: u32,
    manifest_identity: String,
    project_id: String,
    #[serde(default)]
    pub overrides: ProjectOverrides,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trust: Option<ProjectTrust>,
}

impl ProjectPrivateState {
    fn new(definition: &ProjectDefinition) -> Self {
        Self {
            schema_version: PRIVATE_STATE_VERSION,
            manifest_identity: manifest_identity(
                &definition.manifest_path,
                &definition.manifest.id,
            ),
            project_id: definition.manifest.id.clone(),
            overrides: ProjectOverrides::default(),
            trust: None,
        }
    }

    pub fn trust_status(&self, definition: &ProjectDefinition) -> ProjectTrustStatus {
        if !definition.manifest.requires_trust() {
            return ProjectTrustStatus::NotRequired;
        }
        match &self.trust {
            Some(trust)
                if trust.manifest_digest == definition.digest
                    && trust.overrides_digest == overrides_digest(&self.overrides) =>
            {
                ProjectTrustStatus::Trusted
            }
            Some(_) => ProjectTrustStatus::Stale,
            None => ProjectTrustStatus::Untrusted,
        }
    }

    pub fn require_execution_trust(
        &self,
        definition: &ProjectDefinition,
    ) -> Result<(), ProjectError> {
        match self.trust_status(definition) {
            ProjectTrustStatus::NotRequired | ProjectTrustStatus::Trusted => Ok(()),
            ProjectTrustStatus::Untrusted => Err(ProjectError::new(
                "project_manifest_untrusted",
                "review and trust the current manifest before running project commands",
            )),
            ProjectTrustStatus::Stale => Err(ProjectError::new(
                "project_manifest_trust_stale",
                "the manifest or its private overrides changed after it was trusted; review and trust the current definition",
            )),
        }
    }

    pub fn trusted_manifest_digest(&self) -> Option<&str> {
        self.trust
            .as_ref()
            .map(|trust| trust.manifest_digest.as_str())
    }

    fn validate_for(&self, definition: &ProjectDefinition) -> Result<(), ProjectError> {
        if self.schema_version != PRIVATE_STATE_VERSION {
            return Err(ProjectError::new(
                "unsupported_project_private_state",
                format!(
                    "project private state version {} is not supported",
                    self.schema_version
                ),
            ));
        }
        if self.manifest_identity
            != manifest_identity(&definition.manifest_path, &definition.manifest.id)
            || self.project_id != definition.manifest.id
        {
            return Err(ProjectError::new(
                "project_private_state_identity_mismatch",
                "project private state belongs to a different manifest",
            ));
        }
        self.overrides.validate_for(&definition.manifest)?;
        if let Some(trust) = &self.trust {
            if !valid_digest(&trust.manifest_digest) || !valid_digest(&trust.overrides_digest) {
                return Err(ProjectError::new(
                    "invalid_project_private_state",
                    "project trust digest is invalid",
                ));
            }
        }
        Ok(())
    }
}

pub struct ProjectPrivateStateRepository {
    root: PathBuf,
}

impl ProjectPrivateStateRepository {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn in_default_state_dir() -> Self {
        Self::new(crate::config::state_dir().join("projects"))
    }

    pub fn path_for(&self, definition: &ProjectDefinition) -> PathBuf {
        self.root.join(format!(
            "{}.json",
            manifest_identity(&definition.manifest_path, &definition.manifest.id)
        ))
    }

    pub fn load(
        &self,
        definition: &ProjectDefinition,
    ) -> Result<ProjectPrivateState, ProjectError> {
        match fs::symlink_metadata(&self.root) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(ProjectError::new(
                    "invalid_project_private_state_directory",
                    "project private state directory must be a regular directory, not a symlink",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ProjectPrivateState::new(definition));
            }
            Err(error) => return Err(private_io_error("directory metadata", &error)),
        }
        let path = self.path_for(definition);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ProjectPrivateState::new(definition));
            }
            Err(error) => return Err(private_io_error("metadata", &error)),
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_PRIVATE_STATE_BYTES
        {
            return Err(ProjectError::new(
                "invalid_project_private_state",
                "project private state must be a regular JSON file no larger than 1 MiB",
            ));
        }
        restrict_file_permissions(&path)?;
        let file = fs::File::open(&path).map_err(|error| private_io_error("open", &error))?;
        let mut bytes = Vec::with_capacity(metadata.len().min(MAX_PRIVATE_STATE_BYTES) as usize);
        file.take(MAX_PRIVATE_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| private_io_error("read", &error))?;
        if bytes.len() as u64 > MAX_PRIVATE_STATE_BYTES {
            return Err(ProjectError::new(
                "invalid_project_private_state",
                "project private state must be no larger than 1 MiB",
            ));
        }
        let state = serde_json::from_slice::<ProjectPrivateState>(&bytes).map_err(|_| {
            ProjectError::new(
                "invalid_project_private_state",
                "project private state is invalid JSON",
            )
        })?;
        state.validate_for(definition)?;
        Ok(state)
    }
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> Result<(), ProjectError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| private_io_error("file permission update", &error))
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> Result<(), ProjectError> {
    Ok(())
}

fn private_io_error(operation: &'static str, error: &io::Error) -> ProjectError {
    ProjectError::new(
        "project_private_state_io",
        format!(
            "project private state {operation} failed ({:?})",
            error.kind()
        ),
    )
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn manifest_identity(path: &Path, project_id: &str) -> String {
    let mut hasher = Sha256::new();
    let path = manifest_path_bytes(path);
    hasher.update((path.len() as u64).to_le_bytes());
    hasher.update(path);
    hasher.update(project_id.as_bytes());
    digest_to_hex(&hasher.finalize())
}

#[cfg(unix)]
fn manifest_path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn manifest_path_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn manifest_path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

fn overrides_digest(overrides: &ProjectOverrides) -> String {
    let mut hasher = Sha256::new();
    for (repository, base) in &overrides.repository_bases {
        hasher.update((repository.len() as u64).to_le_bytes());
        hasher.update(repository.as_bytes());
        hasher.update((base.len() as u64).to_le_bytes());
        hasher.update(base.as_bytes());
    }
    digest_to_hex(&hasher.finalize())
}

fn digest_to_hex(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for &byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::project::model::{
        ProjectCommand, ProjectManifest, ProjectRepo, PROJECT_MANIFEST_VERSION,
    };

    fn definition(root: &Path, digest: &str, executable: bool) -> ProjectDefinition {
        ProjectDefinition {
            manifest_path: root.join("gowild-project.toml"),
            root: root.to_path_buf(),
            digest: digest.into(),
            manifest: ProjectManifest {
                version: PROJECT_MANIFEST_VERSION,
                id: "sample".into(),
                name: "Sample".into(),
                repositories: vec![ProjectRepo {
                    id: "api".into(),
                    path: "api".into(),
                    base: Some("main".into()),
                    depends_on: Vec::new(),
                }],
                setup: executable
                    .then(|| ProjectCommand {
                        id: "setup".into(),
                        repository: Some("api".into()),
                        cwd: None,
                        argv: vec!["just".into(), "setup".into()],
                        environment: BTreeMap::new(),
                    })
                    .into_iter()
                    .collect(),
                tests: Vec::new(),
                services: Vec::new(),
            },
        }
    }

    fn test_root(name: &str) -> PathBuf {
        static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "gowild-private-state-{name}-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn trust(definition: &ProjectDefinition, state: &ProjectPrivateState) -> ProjectTrust {
        ProjectTrust {
            manifest_digest: definition.digest.clone(),
            overrides_digest: overrides_digest(&state.overrides),
            granted_at_unix_seconds: 1,
        }
    }

    #[test]
    fn manifest_and_override_changes_make_trust_stale() {
        let root = test_root("trust");
        let current = definition(&root, &"a".repeat(64), true);
        let mut state = ProjectPrivateState::new(&current);
        assert_eq!(state.trust_status(&current), ProjectTrustStatus::Untrusted);
        state.trust = Some(trust(&current, &state));
        assert_eq!(state.trust_status(&current), ProjectTrustStatus::Trusted);

        state
            .overrides
            .repository_bases
            .insert("api".into(), "release".into());
        assert_eq!(state.trust_status(&current), ProjectTrustStatus::Stale);
        assert_eq!(
            state.require_execution_trust(&current).unwrap_err().code,
            "project_manifest_trust_stale"
        );

        let changed = definition(&root, &"b".repeat(64), true);
        state.overrides = ProjectOverrides::default();
        assert_eq!(state.trust_status(&changed), ProjectTrustStatus::Stale);
    }

    #[test]
    fn non_executable_projects_do_not_require_trust() {
        let root = test_root("trust-not-required");
        let definition = definition(&root, &"a".repeat(64), false);
        let state = ProjectPrivateState::new(&definition);

        assert_eq!(
            state.trust_status(&definition),
            ProjectTrustStatus::NotRequired
        );
        state.require_execution_trust(&definition).unwrap();
    }

    #[test]
    fn trust_requires_exact_digest_and_revocation_is_idempotent() {
        let root = test_root("trust-mutation");
        let definition = definition(&root, &"a".repeat(64), true);
        let mut state = ProjectPrivateState::new(&definition);

        assert_eq!(
            state
                .grant_trust(&definition, &"b".repeat(64))
                .unwrap_err()
                .code,
            "project_trust_digest_mismatch"
        );
        state.grant_trust(&definition, &definition.digest).unwrap();
        assert_eq!(state.trust_status(&definition), ProjectTrustStatus::Trusted);
        assert!(state.revoke_trust());
        assert!(!state.revoke_trust());
        assert_eq!(
            state.trust_status(&definition),
            ProjectTrustStatus::Untrusted
        );
    }

    #[test]
    fn repository_atomically_replaces_owner_only_state() {
        let root = test_root("write");
        let definition = definition(&root, &"a".repeat(64), true);
        let repository = ProjectPrivateStateRepository::new(root.join("state"));
        let mut state = ProjectPrivateState::new(&definition);
        state
            .overrides
            .repository_bases
            .insert("api".into(), "release".into());
        repository.save(&definition, &state).unwrap();

        state
            .overrides
            .repository_bases
            .insert("api".into(), "main".into());
        repository.save(&definition, &state).unwrap();
        assert_eq!(repository.load(&definition).unwrap(), state);
        assert!(fs::read_dir(&repository.root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&repository.root).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(repository.path_for(&definition))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_identity_is_rejected_before_writing() {
        let root = test_root("write-identity");
        let definition = definition(&root, &"a".repeat(64), false);
        let repository = ProjectPrivateStateRepository::new(root.join("state"));
        let mut state = ProjectPrivateState::new(&definition);
        state.project_id = "another-project".into();

        assert_eq!(
            repository.save(&definition, &state).unwrap_err().code,
            "project_private_state_identity_mismatch"
        );
        assert!(!repository.root.exists());
    }

    #[test]
    fn repository_loads_valid_state_and_restricts_file_permissions() {
        let root = test_root("valid");
        let definition = definition(&root, &"a".repeat(64), true);
        let repository = ProjectPrivateStateRepository::new(root.join("state"));
        fs::create_dir_all(&repository.root).unwrap();
        let mut state = ProjectPrivateState::new(&definition);
        state.trust = Some(trust(&definition, &state));
        let path = repository.path_for(&definition);
        fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

        assert_eq!(repository.load(&definition).unwrap(), state);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_unknown_and_oversized_state_is_rejected() {
        let root = test_root("invalid");
        let definition = definition(&root, &"a".repeat(64), false);
        let repository = ProjectPrivateStateRepository::new(root.join("state"));
        fs::create_dir_all(&repository.root).unwrap();
        let path = repository.path_for(&definition);

        fs::write(&path, b"not json").unwrap();
        assert_eq!(
            repository.load(&definition).unwrap_err().code,
            "invalid_project_private_state"
        );

        let mut value = serde_json::to_value(ProjectPrivateState::new(&definition)).unwrap();
        value["unexpected"] = serde_json::Value::Bool(true);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            repository.load(&definition).unwrap_err().code,
            "invalid_project_private_state"
        );

        fs::write(&path, vec![b' '; MAX_PRIVATE_STATE_BYTES as usize + 1]).unwrap();
        assert_eq!(
            repository.load(&definition).unwrap_err().code,
            "invalid_project_private_state"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_state_identity_mismatch_is_rejected() {
        let root = test_root("identity");
        let definition = definition(&root, &"a".repeat(64), false);
        let mut state = ProjectPrivateState::new(&definition);
        state.project_id = "another-project".into();

        assert_eq!(
            state.validate_for(&definition).unwrap_err().code,
            "project_private_state_identity_mismatch"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_private_state_file_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = test_root("symlink");
        let definition = definition(&root, &"a".repeat(64), false);
        let repository = ProjectPrivateStateRepository::new(root.join("state"));
        fs::create_dir_all(&repository.root).unwrap();
        let target = root.join("target.json");
        fs::write(&target, "leave unchanged").unwrap();
        symlink(&target, repository.path_for(&definition)).unwrap();

        assert_eq!(
            repository.load(&definition).unwrap_err().code,
            "invalid_project_private_state"
        );
        assert_eq!(
            repository
                .save(&definition, &ProjectPrivateState::new(&definition))
                .unwrap_err()
                .code,
            "invalid_project_private_state"
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "leave unchanged");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_private_state_directory_is_rejected_without_following_it() {
        use std::os::unix::fs::symlink;

        let root = test_root("directory-symlink");
        let definition = definition(&root, &"a".repeat(64), false);
        let repository = ProjectPrivateStateRepository::new(root.join("state"));
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &repository.root).unwrap();

        assert_eq!(
            repository.load(&definition).unwrap_err().code,
            "invalid_project_private_state_directory"
        );
        assert_eq!(
            repository
                .save(&definition, &ProjectPrivateState::new(&definition))
                .unwrap_err()
                .code,
            "invalid_project_private_state_directory"
        );
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
