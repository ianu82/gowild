use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;

use crate::gateway::{redact, Credential, GatewayProtocol};
use crate::pane::{PaneEnvValue, PaneLaunchEnv};

use super::{AdapterRegistry, GatewayResolutionError, GatewayResolver, ResolvedGateway};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CodingCli {
    Codex,
    Claude,
}

impl CodingCli {
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    pub(crate) fn from_id(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            _ => None,
        }
    }
}

impl fmt::Display for CodingCli {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.id())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchMode {
    Fresh,
    Resume { session_ref: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchRequest {
    pub(crate) gateway_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) mode: LaunchMode,
    pub(crate) passthrough_args: Vec<OsString>,
}

impl Default for LaunchRequest {
    fn default() -> Self {
        Self {
            gateway_id: None,
            model: None,
            mode: LaunchMode::Fresh,
            passthrough_args: Vec::new(),
        }
    }
}

pub(crate) trait CliAdapter {
    fn cli(&self) -> CodingCli;
    #[allow(dead_code)]
    fn display_name(&self) -> &'static str;
    fn executable_candidates(&self) -> &'static [&'static str];
    fn required_protocol(&self) -> GatewayProtocol;
    fn build(
        &self,
        executable: &Path,
        resolved: &ResolvedGateway,
        request: &LaunchRequest,
    ) -> Result<LaunchSpec, AdapterError>;
}

pub(crate) struct AdapterError {
    message: String,
}

impl AdapterError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Debug for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AdapterError(<sanitized by launch planner>)")
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AdapterError {}

#[derive(Clone)]
pub(crate) enum ChildEnvironmentValue {
    Plain(String),
    Secret(Credential),
}

impl ChildEnvironmentValue {
    #[cfg(test)]
    fn expose(&self) -> &str {
        match self {
            Self::Plain(value) => value,
            Self::Secret(value) => value.expose(),
        }
    }
}

impl fmt::Debug for ChildEnvironmentValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, Default)]
pub(crate) struct ChildEnvironment {
    set: BTreeMap<String, ChildEnvironmentValue>,
    remove: BTreeSet<String>,
}

impl ChildEnvironment {
    pub(crate) fn set_plain(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), LaunchSpecError> {
        self.set_value(key.into(), ChildEnvironmentValue::Plain(value.into()))
    }

    pub(crate) fn set_secret(
        &mut self,
        key: impl Into<String>,
        value: Credential,
    ) -> Result<(), LaunchSpecError> {
        self.set_value(key.into(), ChildEnvironmentValue::Secret(value))
    }

    fn set_value(
        &mut self,
        key: String,
        value: ChildEnvironmentValue,
    ) -> Result<(), LaunchSpecError> {
        validate_environment_key(&key)?;
        if self.remove.contains(&key) {
            return Err(LaunchSpecError::ConflictingEnvironmentKey);
        }
        self.set.insert(key, value);
        Ok(())
    }

    pub(crate) fn remove(&mut self, key: impl Into<String>) -> Result<(), LaunchSpecError> {
        let key = key.into();
        validate_environment_key(&key)?;
        if self.set.contains_key(&key) {
            return Err(LaunchSpecError::ConflictingEnvironmentKey);
        }
        self.remove.insert(key);
        Ok(())
    }

    #[cfg(test)]
    fn apply_to(&self, command: &mut Command) {
        for key in &self.remove {
            command.env_remove(key);
        }
        for (key, value) in &self.set {
            command.env(key, value.expose());
        }
    }

    pub(crate) fn into_pane_launch_env(self) -> PaneLaunchEnv {
        let extra = self
            .set
            .into_iter()
            .map(|(key, value)| {
                let value = match value {
                    ChildEnvironmentValue::Plain(value) => PaneEnvValue::Plain(value),
                    ChildEnvironmentValue::Secret(value) => PaneEnvValue::Secret(value),
                };
                (key, value)
            })
            .collect();
        PaneLaunchEnv::from_changes(extra, self.remove.into_iter().collect())
    }

    fn contains_plain_value(&self, expected: &str) -> bool {
        self.set
            .values()
            .any(|value| matches!(value, ChildEnvironmentValue::Plain(value) if value.contains(expected)))
    }

    fn contains_secret_component(&self, expected: &str) -> bool {
        self.set.values().any(
            |value| matches!(value, ChildEnvironmentValue::Secret(value) if value.expose().contains(expected)),
        )
    }
}

impl fmt::Debug for ChildEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChildEnvironment")
            .field(
                "set",
                &self
                    .set
                    .keys()
                    .map(|key| (key, "[REDACTED]"))
                    .collect::<Vec<_>>(),
            )
            .field("remove", &self.remove)
            .finish()
    }
}

#[derive(Clone)]
pub(crate) struct LaunchSpec {
    pub(crate) cli: CodingCli,
    pub(crate) executable: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) environment: ChildEnvironment,
}

impl LaunchSpec {
    pub(crate) fn new(
        cli: CodingCli,
        executable: PathBuf,
        args: Vec<OsString>,
        environment: ChildEnvironment,
    ) -> Self {
        Self {
            cli,
            executable,
            args,
            environment,
        }
    }

    #[cfg(test)]
    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command.args(&self.args);
        self.environment.apply_to(&mut command);
        command
    }

    pub(crate) fn into_pane_parts(self) -> (PathBuf, Vec<OsString>, PaneLaunchEnv) {
        (
            self.executable,
            self.args,
            self.environment.into_pane_launch_env(),
        )
    }

    fn validate(
        &self,
        expected_cli: CodingCli,
        resolved: &ResolvedGateway,
    ) -> Result<(), LaunchSpecError> {
        if self.cli != expected_cli {
            return Err(LaunchSpecError::WrongCli);
        }
        if self.executable.as_os_str().is_empty() {
            return Err(LaunchSpecError::MissingExecutable);
        }

        match &resolved.credential {
            Some(credential) => {
                let exposed = credential.expose();
                if self
                    .args
                    .iter()
                    .any(|arg| arg.to_string_lossy().contains(exposed))
                    || self.environment.contains_plain_value(exposed)
                {
                    return Err(LaunchSpecError::ExposedCredential);
                }
                // Some protocols require a fixed prefix to be applied to a
                // credential inside a secret-bearing header. The original
                // credential must still be wholly contained in a value that
                // remains classified as secret.
                if !self.environment.contains_secret_component(exposed) {
                    return Err(LaunchSpecError::MissingSecretCredential);
                }
            }
            None => {
                if self
                    .environment
                    .set
                    .values()
                    .any(|value| matches!(value, ChildEnvironmentValue::Secret(_)))
                {
                    return Err(LaunchSpecError::UnexpectedSecretCredential);
                }
            }
        }
        Ok(())
    }
}

impl fmt::Debug for LaunchSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LaunchSpec")
            .field("cli", &self.cli)
            .field("executable", &self.executable)
            .field("args", &self.args)
            .field("environment", &self.environment)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchSpecError {
    InvalidEnvironmentKey,
    ConflictingEnvironmentKey,
    WrongCli,
    MissingExecutable,
    ExposedCredential,
    MissingSecretCredential,
    UnexpectedSecretCredential,
}

impl fmt::Display for LaunchSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidEnvironmentKey => "the launch environment contains an invalid key",
            Self::ConflictingEnvironmentKey => {
                "the launch environment cannot set and remove the same key"
            }
            Self::WrongCli => "the adapter returned a launch for the wrong CLI",
            Self::MissingExecutable => "the adapter returned an empty executable path",
            Self::ExposedCredential => {
                "the adapter exposed a gateway credential outside the secret environment"
            }
            Self::MissingSecretCredential => {
                "the adapter omitted the gateway credential from the secret environment"
            }
            Self::UnexpectedSecretCredential => {
                "the adapter supplied a secret for an unauthenticated gateway"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for LaunchSpecError {}

fn validate_environment_key(key: &str) -> Result<(), LaunchSpecError> {
    if key.is_empty() || key.bytes().any(|byte| byte == b'=' || byte == b'\0') {
        Err(LaunchSpecError::InvalidEnvironmentKey)
    } else {
        Ok(())
    }
}

pub(crate) trait ExecutableLocator {
    fn locate(&self, candidates: &[&str]) -> Option<PathBuf>;
}

pub(crate) struct PathExecutableLocator;

impl ExecutableLocator for PathExecutableLocator {
    fn locate(&self, candidates: &[&str]) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        for directory in std::env::split_paths(&path) {
            for candidate in candidates {
                if let Some(found) = executable_in(&directory, candidate) {
                    return Some(found);
                }
            }
        }
        None
    }
}

fn executable_in(directory: &Path, candidate: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let candidate_path = Path::new(candidate);
        if candidate_path.extension().is_some() {
            let path = directory.join(candidate_path);
            return path.is_file().then_some(path);
        }
        let extensions =
            std::env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
        for extension in extensions.to_string_lossy().split(';') {
            let path = directory.join(format!("{candidate}{extension}"));
            if path.is_file() {
                return Some(path);
            }
        }
        None
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join(candidate);
        let metadata = path.metadata().ok()?;
        (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then_some(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let path = directory.join(candidate);
        path.is_file().then_some(path)
    }
}

pub(crate) struct LaunchPlanner<'a> {
    registry: &'a AdapterRegistry,
    resolver: GatewayResolver<'a>,
    locator: &'a dyn ExecutableLocator,
}

impl<'a> LaunchPlanner<'a> {
    pub(crate) fn new(
        registry: &'a AdapterRegistry,
        resolver: GatewayResolver<'a>,
        locator: &'a dyn ExecutableLocator,
    ) -> Self {
        Self {
            registry,
            resolver,
            locator,
        }
    }

    #[cfg(test)]
    pub(crate) fn plan(
        &self,
        cli: CodingCli,
        request: &LaunchRequest,
    ) -> Result<LaunchSpec, LaunchError> {
        let resolved = self.resolve(cli, request)?;
        self.plan_resolved(cli, request, &resolved)
    }

    pub(crate) fn resolve(
        &self,
        cli: CodingCli,
        request: &LaunchRequest,
    ) -> Result<ResolvedGateway, LaunchError> {
        let adapter = self.registry.get(cli).ok_or(LaunchError::UnknownCli(cli))?;
        self.resolver
            .resolve(
                cli.id(),
                adapter.required_protocol(),
                request.gateway_id.as_deref(),
                request.model.as_deref(),
            )
            .map_err(LaunchError::Gateway)
    }

    pub(crate) fn plan_resolved(
        &self,
        cli: CodingCli,
        request: &LaunchRequest,
        resolved: &ResolvedGateway,
    ) -> Result<LaunchSpec, LaunchError> {
        let adapter = self.registry.get(cli).ok_or(LaunchError::UnknownCli(cli))?;
        let executable = self
            .locator
            .locate(adapter.executable_candidates())
            .ok_or(LaunchError::ExecutableNotFound(cli))?;
        let spec = adapter
            .build(&executable, resolved, request)
            .map_err(|error| LaunchError::Adapter {
                cli,
                message: redact(
                    &error.to_string(),
                    &resolved.credential.iter().collect::<Vec<_>>(),
                ),
            })?;
        spec.validate(cli, resolved).map_err(LaunchError::Spec)?;
        Ok(spec)
    }
}

#[derive(Debug)]
pub(crate) enum LaunchError {
    UnknownCli(CodingCli),
    ExecutableNotFound(CodingCli),
    Gateway(GatewayResolutionError),
    Adapter { cli: CodingCli, message: String },
    Spec(LaunchSpecError),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCli(cli) => write!(formatter, "no adapter is registered for {cli}"),
            Self::ExecutableNotFound(cli) => {
                write!(formatter, "the {cli} executable was not found on PATH")
            }
            Self::Gateway(error) => write!(formatter, "gateway resolution failed: {error}"),
            Self::Adapter { cli, message } => write!(formatter, "{cli} adapter failed: {message}"),
            Self::Spec(error) => write!(formatter, "invalid launch specification: {error}"),
        }
    }
}

impl std::error::Error for LaunchError {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::cli_adapter::resolver::ENV_API_KEY;
    use crate::cli_adapter::Environment;
    use crate::gateway::{
        CredentialBackend, CredentialStore, CredentialStoreError, GatewayCatalog,
    };

    use super::*;

    #[derive(Default)]
    struct MapEnvironment(BTreeMap<String, String>);

    impl Environment for MapEnvironment {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    #[derive(Default)]
    struct MemoryCredentials;

    impl CredentialStore for MemoryCredentials {
        fn get(&self, _credential_ref: &str) -> Result<Option<Credential>, CredentialStoreError> {
            Ok(None)
        }

        fn set(
            &self,
            _credential_ref: &str,
            _credential: &Credential,
        ) -> Result<CredentialBackend, CredentialStoreError> {
            unreachable!()
        }

        fn delete(&self, _credential_ref: &str) -> Result<(), CredentialStoreError> {
            unreachable!()
        }
    }

    struct FakeLocator(PathBuf);

    impl ExecutableLocator for FakeLocator {
        fn locate(&self, _candidates: &[&str]) -> Option<PathBuf> {
            Some(self.0.clone())
        }
    }

    struct FakeAdapter;

    impl CliAdapter for FakeAdapter {
        fn cli(&self) -> CodingCli {
            CodingCli::Codex
        }

        fn display_name(&self) -> &'static str {
            "Fake Codex"
        }

        fn executable_candidates(&self) -> &'static [&'static str] {
            &["fake-codex"]
        }

        fn required_protocol(&self) -> GatewayProtocol {
            GatewayProtocol::OpenAiResponses
        }

        fn build(
            &self,
            _executable: &Path,
            resolved: &ResolvedGateway,
            request: &LaunchRequest,
        ) -> Result<LaunchSpec, AdapterError> {
            let mut environment = ChildEnvironment::default();
            environment
                .set_plain("FAKE_BASE_URL", &resolved.endpoint)
                .map_err(|error| AdapterError::new(error.to_string()))?;
            if let Some(credential) = &resolved.credential {
                environment
                    .set_secret("FAKE_API_KEY", credential.clone())
                    .map_err(|error| AdapterError::new(error.to_string()))?;
            }
            let mode = match &request.mode {
                LaunchMode::Fresh => "fresh".into(),
                LaunchMode::Resume { session_ref } => format!("resume:{session_ref}"),
            };
            Ok(LaunchSpec::new(
                CodingCli::Codex,
                _executable.into(),
                vec![mode.into()],
                environment,
            ))
        }
    }

    fn planner<'a>(
        registry: &'a AdapterRegistry,
        catalog: &'a GatewayCatalog,
        credentials: &'a MemoryCredentials,
        environment: &'a MapEnvironment,
        locator: &'a FakeLocator,
    ) -> LaunchPlanner<'a> {
        LaunchPlanner::new(
            registry,
            GatewayResolver::new(catalog, credentials, environment),
            locator,
        )
    }

    fn configured_catalog() -> GatewayCatalog {
        let mut catalog = GatewayCatalog::with_builtin_presets();
        catalog.default_gateway_id = Some("mindshub".into());
        catalog
    }

    #[test]
    fn environment_rejects_invalid_or_conflicting_keys() {
        let mut environment = ChildEnvironment::default();
        assert_eq!(
            environment.set_plain("", "value"),
            Err(LaunchSpecError::InvalidEnvironmentKey)
        );
        environment.remove("OLD_KEY").unwrap();
        assert_eq!(
            environment.set_plain("OLD_KEY", "value"),
            Err(LaunchSpecError::ConflictingEnvironmentKey)
        );
    }

    #[test]
    fn planner_rejects_credentials_in_plain_environment() {
        struct UnsafeAdapter;
        impl CliAdapter for UnsafeAdapter {
            fn cli(&self) -> CodingCli {
                CodingCli::Codex
            }
            fn display_name(&self) -> &'static str {
                "Unsafe"
            }
            fn executable_candidates(&self) -> &'static [&'static str] {
                &["unsafe"]
            }
            fn required_protocol(&self) -> GatewayProtocol {
                GatewayProtocol::OpenAiResponses
            }
            fn build(
                &self,
                executable: &Path,
                resolved: &ResolvedGateway,
                _request: &LaunchRequest,
            ) -> Result<LaunchSpec, AdapterError> {
                let mut environment = ChildEnvironment::default();
                environment
                    .set_plain(
                        "UNSAFE_API_KEY",
                        resolved.credential.as_ref().unwrap().expose(),
                    )
                    .unwrap();
                Ok(LaunchSpec::new(
                    CodingCli::Codex,
                    executable.into(),
                    Vec::new(),
                    environment,
                ))
            }
        }

        let mut registry = AdapterRegistry::default();
        registry.register(UnsafeAdapter).unwrap();
        let catalog = configured_catalog();
        let credentials = MemoryCredentials;
        let environment = MapEnvironment(BTreeMap::from([(
            ENV_API_KEY.into(),
            "unsafe-fake-key".into(),
        )]));
        let locator = FakeLocator("unsafe".into());
        let error = planner(&registry, &catalog, &credentials, &environment, &locator)
            .plan(CodingCli::Codex, &LaunchRequest::default())
            .unwrap_err();
        assert!(matches!(
            error,
            LaunchError::Spec(LaunchSpecError::ExposedCredential)
        ));
        assert!(!format!("{error:?}").contains("unsafe-fake-key"));
    }

    #[cfg(unix)]
    #[test]
    fn fresh_and_resume_launches_inject_the_same_gateway_only_into_the_child() {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);

        let directory = std::env::temp_dir().join(format!(
            "gowild-launch-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let executable = directory.join("fake-codex");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"$1\" \"$FAKE_BASE_URL\" \"$FAKE_API_KEY\"\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();

        let mut registry = AdapterRegistry::default();
        registry.register(FakeAdapter).unwrap();
        let catalog = configured_catalog();
        let credentials = MemoryCredentials;
        let environment = MapEnvironment(BTreeMap::from([(
            ENV_API_KEY.into(),
            "child-only-fake-key".into(),
        )]));
        let locator = FakeLocator(executable);
        let planner = planner(&registry, &catalog, &credentials, &environment, &locator);
        let parent_value = std::env::var_os("FAKE_API_KEY");

        let fresh = planner
            .plan(CodingCli::Codex, &LaunchRequest::default())
            .unwrap();
        let resumed = planner
            .plan(
                CodingCli::Codex,
                &LaunchRequest {
                    mode: LaunchMode::Resume {
                        session_ref: "session-42".into(),
                    },
                    ..LaunchRequest::default()
                },
            )
            .unwrap();
        for (spec, expected_mode) in [(fresh, "fresh"), (resumed, "resume:session-42")] {
            let debug = format!("{spec:?}");
            assert!(!debug.contains("child-only-fake-key"));
            assert!(spec
                .args
                .iter()
                .all(|argument| argument != std::ffi::OsStr::new("child-only-fake-key")));
            let output = spec.command().output().unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!("{expected_mode}\nhttps://api.mindshub.ai/v1\nchild-only-fake-key\n")
            );
        }
        assert_eq!(std::env::var_os("FAKE_API_KEY"), parent_value);

        fs::remove_file(directory.join("fake-codex")).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
