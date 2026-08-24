use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PROJECT_MANIFEST_FILE: &str = "gowild-project.toml";
pub const PROJECT_MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectError {
    pub code: &'static str,
    pub message: String,
}

impl ProjectError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProjectError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectManifest {
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(rename = "repository")]
    pub repositories: Vec<ProjectRepo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<ProjectCommand>,
    #[serde(default, rename = "test", skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<ProjectCommand>,
    #[serde(default, rename = "service", skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ProjectService>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRepo {
    pub id: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCommand {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectService {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub isolation: RuntimeIsolationSpec,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIsolationSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub containers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub databases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caches: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub compose: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ProjectManifest {
    pub fn validate(&self) -> Result<(), ProjectError> {
        let mut diagnostics = Vec::new();
        if self.version != PROJECT_MANIFEST_VERSION {
            diagnostics.push(format!(
                "version must be {PROJECT_MANIFEST_VERSION}, got {}",
                self.version
            ));
        }
        validate_id("project id", &self.id, &mut diagnostics);
        validate_name("project name", &self.name, &mut diagnostics);
        if self.repositories.is_empty() {
            diagnostics.push("at least one [[repository]] is required".to_string());
        }

        let mut repo_ids = BTreeSet::new();
        let mut repo_paths = BTreeSet::new();
        for repo in &self.repositories {
            validate_id("repository id", &repo.id, &mut diagnostics);
            if !repo_ids.insert(repo.id.as_str()) {
                diagnostics.push(format!("repository id '{}' is duplicated", repo.id));
            }
            validate_relative_path(
                &format!("repository '{}' path", repo.id),
                &repo.path,
                true,
                &mut diagnostics,
            );
            let normalized_path = normalized_relative_key(&repo.path);
            if !repo_paths.insert(normalized_path) {
                diagnostics.push(format!(
                    "repository '{}' repeats path '{}'",
                    repo.id,
                    repo.path.display()
                ));
            }
            if let Some(base) = &repo.base {
                if base.trim().is_empty()
                    || base.len() > 255
                    || base.starts_with('-')
                    || base.chars().any(char::is_control)
                {
                    diagnostics.push(format!(
                        "repository '{}' base must be a safe, non-empty Git ref",
                        repo.id
                    ));
                }
            }
            let mut dependencies = BTreeSet::new();
            for dependency in &repo.depends_on {
                if !dependencies.insert(dependency) {
                    diagnostics.push(format!(
                        "repository '{}' repeats dependency '{dependency}'",
                        repo.id
                    ));
                }
            }
        }

        for repo in &self.repositories {
            for dependency in &repo.depends_on {
                if dependency == &repo.id {
                    diagnostics.push(format!("repository '{}' depends on itself", repo.id));
                } else if !repo_ids.contains(dependency.as_str()) {
                    diagnostics.push(format!(
                        "repository '{}' depends on unknown repository '{dependency}'",
                        repo.id
                    ));
                }
            }
        }

        let mut command_ids = BTreeSet::new();
        for (kind, command) in self
            .setup
            .iter()
            .map(|command| ("setup", command))
            .chain(self.tests.iter().map(|command| ("test", command)))
        {
            validate_command(kind, command, &repo_ids, &mut diagnostics);
            if !command_ids.insert(command.id.as_str()) {
                diagnostics.push(format!("command id '{}' is duplicated", command.id));
            }
        }

        let mut service_ids = BTreeSet::new();
        let mut compose_services = 0usize;
        for service in &self.services {
            validate_id("service id", &service.id, &mut diagnostics);
            if !service_ids.insert(service.id.as_str()) {
                diagnostics.push(format!("service id '{}' is duplicated", service.id));
            }
            validate_execution(
                "service",
                &service.id,
                service.repository.as_deref(),
                service.cwd.as_deref(),
                &service.argv,
                &service.environment,
                &repo_ids,
                &mut diagnostics,
            );
            validate_resource_names(
                &service.id,
                "port",
                &service.isolation.ports,
                &mut diagnostics,
            );
            validate_resource_names(
                &service.id,
                "container",
                &service.isolation.containers,
                &mut diagnostics,
            );
            validate_resource_names(
                &service.id,
                "database",
                &service.isolation.databases,
                &mut diagnostics,
            );
            validate_resource_names(
                &service.id,
                "data",
                &service.isolation.data,
                &mut diagnostics,
            );
            validate_resource_names(
                &service.id,
                "cache",
                &service.isolation.caches,
                &mut diagnostics,
            );
            if service.isolation.compose {
                compose_services = compose_services.saturating_add(1);
                if let Err(message) = validate_compose_command(&service.argv) {
                    diagnostics.push(format!("Compose service '{}': {message}", service.id));
                }
            }
        }
        if compose_services > 1 {
            diagnostics.push(
                "a project may declare at most one Compose service; one stack can contain multiple containers"
                    .to_string(),
            );
        }

        if diagnostics.is_empty() {
            self.dependency_order()?;
            Ok(())
        } else {
            Err(ProjectError::new(
                "invalid_project_manifest",
                diagnostics.join("; "),
            ))
        }
    }

    pub fn dependency_order(&self) -> Result<Vec<String>, ProjectError> {
        let repo_by_id: HashMap<&str, &ProjectRepo> = self
            .repositories
            .iter()
            .map(|repo| (repo.id.as_str(), repo))
            .collect();
        let mut indegree: HashMap<&str, usize> = self
            .repositories
            .iter()
            .map(|repo| (repo.id.as_str(), repo.depends_on.len()))
            .collect();
        let mut dependants: HashMap<&str, Vec<&str>> = HashMap::new();
        for repo in &self.repositories {
            for dependency in &repo.depends_on {
                if repo_by_id.contains_key(dependency.as_str()) {
                    dependants
                        .entry(dependency.as_str())
                        .or_default()
                        .push(repo.id.as_str());
                }
            }
        }

        let mut ready = self
            .repositories
            .iter()
            .filter(|repo| indegree.get(repo.id.as_str()) == Some(&0))
            .map(|repo| repo.id.as_str())
            .collect::<VecDeque<_>>();
        let mut ordered = Vec::with_capacity(self.repositories.len());
        while let Some(repo_id) = ready.pop_front() {
            ordered.push(repo_id.to_string());
            if let Some(children) = dependants.get(repo_id) {
                for child in children {
                    if let Some(value) = indegree.get_mut(child) {
                        *value = value.saturating_sub(1);
                        if *value == 0 {
                            ready.push_back(child);
                        }
                    }
                }
            }
        }
        if ordered.len() == self.repositories.len() {
            Ok(ordered)
        } else {
            let blocked = self
                .repositories
                .iter()
                .filter(|repo| !ordered.iter().any(|id| id == &repo.id))
                .map(|repo| repo.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(ProjectError::new(
                "project_dependency_cycle",
                format!("repository dependency cycle includes: {blocked}"),
            ))
        }
    }

    pub fn requires_trust(&self) -> bool {
        !self.setup.is_empty() || !self.tests.is_empty() || !self.services.is_empty()
    }
}

fn validate_command(
    kind: &str,
    command: &ProjectCommand,
    repo_ids: &BTreeSet<&str>,
    diagnostics: &mut Vec<String>,
) {
    validate_id(&format!("{kind} command id"), &command.id, diagnostics);
    validate_execution(
        kind,
        &command.id,
        command.repository.as_deref(),
        command.cwd.as_deref(),
        &command.argv,
        &command.environment,
        repo_ids,
        diagnostics,
    );
}

#[allow(clippy::too_many_arguments)]
fn validate_execution(
    kind: &str,
    id: &str,
    repository: Option<&str>,
    cwd: Option<&Path>,
    argv: &[String],
    environment: &BTreeMap<String, String>,
    repo_ids: &BTreeSet<&str>,
    diagnostics: &mut Vec<String>,
) {
    if let Some(repository) = repository {
        if !repo_ids.contains(repository) {
            diagnostics.push(format!(
                "{kind} '{id}' references unknown repository '{repository}'"
            ));
        }
    }
    if let Some(cwd) = cwd {
        validate_relative_path(&format!("{kind} '{id}' cwd"), cwd, true, diagnostics);
    }
    if argv.first().is_none_or(|program| program.trim().is_empty()) {
        diagnostics.push(format!("{kind} '{id}' argv must start with a program"));
    }
    if argv.iter().any(|argument| argument.contains('\0')) {
        diagnostics.push(format!("{kind} '{id}' argv must not contain NUL bytes"));
    }
    for (key, value) in environment {
        if !valid_environment_key(key) {
            diagnostics.push(format!(
                "{kind} '{id}' environment key '{key}' is not portable"
            ));
        }
        if reserved_runtime_environment_key(key) {
            diagnostics.push(format!(
                "{kind} '{id}' may not override task runtime environment value '{key}'"
            ));
        }
        if secret_like_key(key) && !value.is_empty() {
            diagnostics.push(format!(
                "{kind} '{id}' environment key '{key}' looks secret; store credentials outside the project manifest"
            ));
        }
        if value.contains('\0') {
            diagnostics.push(format!(
                "{kind} '{id}' environment value for '{key}' contains a NUL byte"
            ));
        }
    }
}

fn reserved_runtime_environment_key(key: &str) -> bool {
    key.starts_with("GOWILD_")
        || matches!(
            key,
            "COMPOSE_PROJECT_NAME"
                | "COMPOSE_FILE"
                | "COMPOSE_PATH_SEPARATOR"
                | "COMPOSE_ENV_FILES"
                | "TMPDIR"
                | "TMP"
                | "TEMP"
                | "XDG_CACHE_HOME"
                | "XDG_DATA_HOME"
        )
}

pub(crate) fn validate_compose_command(argv: &[String]) -> Result<usize, &'static str> {
    let compose_arguments_start = match argv.first().map(|value| executable_name(value)) {
        Some("docker" | "docker.exe") if argv.get(1).map(String::as_str) == Some("compose") => 2,
        Some("docker-compose" | "docker-compose.exe") => 1,
        _ => return Err(
            "argv must invoke docker compose or docker-compose directly without a shell wrapper",
        ),
    };
    let arguments = &argv[compose_arguments_start..];
    if arguments.iter().any(|argument| {
        argument == "--project-name"
            || argument.starts_with("--project-name=")
            || argument == "-p"
            || argument
                .strip_prefix("-p")
                .is_some_and(|suffix| !suffix.is_empty())
    }) {
        return Err("argv may not override GoWild's task-scoped Compose project name");
    }
    if arguments.iter().any(|argument| {
        argument == "--project-directory" || argument.starts_with("--project-directory=")
    }) {
        return Err("argv may not override the isolated task working directory");
    }
    let mut up_positions = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument == "up").then_some(index));
    let Some(relative_up_index) = up_positions.next() else {
        return Err("argv must use the up subcommand");
    };
    if up_positions.next().is_some() {
        return Err("argv contains an ambiguous repeated up subcommand");
    }
    let up_index = compose_arguments_start + relative_up_index;
    if !argv[up_index + 1..]
        .iter()
        .any(|argument| matches!(argument.as_str(), "-d" | "--detach"))
    {
        return Err("argv must run Compose in detached mode with -d or --detach");
    }
    Ok(up_index)
}

fn executable_name(value: &str) -> &str {
    value.rsplit(['/', '\\']).next().unwrap_or(value)
}

fn validate_resource_names(
    service_id: &str,
    kind: &str,
    values: &[String],
    diagnostics: &mut Vec<String>,
) {
    let mut seen = BTreeSet::new();
    for value in values {
        validate_id(
            &format!("service '{service_id}' {kind} resource"),
            value,
            diagnostics,
        );
        if !seen.insert(value) {
            diagnostics.push(format!(
                "service '{service_id}' repeats {kind} resource '{value}'"
            ));
        }
    }
}

fn validate_id(label: &str, value: &str, diagnostics: &mut Vec<String>) {
    let valid = !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.ends_with('-')
        && !value.contains("--");
    if !valid {
        diagnostics.push(format!(
            "{label} '{value}' must use 1-63 lowercase letters, digits or single interior hyphens"
        ));
    }
}

fn validate_name(label: &str, value: &str, diagnostics: &mut Vec<String>) {
    if value.trim().is_empty() || value.chars().count() > 120 || value.chars().any(char::is_control)
    {
        diagnostics.push(format!("{label} must contain 1-120 printable characters"));
    }
}

fn validate_relative_path(
    label: &str,
    value: &Path,
    allow_current: bool,
    diagnostics: &mut Vec<String>,
) {
    if value.as_os_str().is_empty() || value.is_absolute() {
        diagnostics.push(format!("{label} must be a relative path"));
        return;
    }
    let mut has_normal_component = false;
    for component in value.components() {
        match component {
            Component::Normal(_) => has_normal_component = true,
            Component::CurDir if allow_current => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                diagnostics.push(format!(
                    "{label} '{}' may not escape its project/repository root",
                    value.display()
                ));
                return;
            }
            Component::CurDir => {}
        }
    }
    if !(has_normal_component || allow_current && value == Path::new(".")) {
        diagnostics.push(format!("{label} must identify a path"));
    }
}

fn normalized_relative_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn valid_environment_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_uppercase())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn secret_like_key(key: &str) -> bool {
    [
        "API_KEY",
        "CREDENTIAL",
        "PASSWORD",
        "PRIVATE_KEY",
        "SECRET",
        "TOKEN",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(repositories: Vec<ProjectRepo>) -> ProjectManifest {
        ProjectManifest {
            version: PROJECT_MANIFEST_VERSION,
            id: "sample-project".into(),
            name: "Sample project".into(),
            repositories,
            setup: Vec::new(),
            tests: Vec::new(),
            services: Vec::new(),
        }
    }

    fn repo(id: &str, dependencies: &[&str]) -> ProjectRepo {
        ProjectRepo {
            id: id.into(),
            path: PathBuf::from(id),
            base: Some("main".into()),
            depends_on: dependencies.iter().map(|value| (*value).into()).collect(),
        }
    }

    fn compose_service(id: &str, argv: &[&str]) -> ProjectService {
        ProjectService {
            id: id.into(),
            repository: Some("api".into()),
            cwd: None,
            argv: argv.iter().map(|value| (*value).into()).collect(),
            environment: BTreeMap::new(),
            isolation: RuntimeIsolationSpec {
                compose: true,
                ..RuntimeIsolationSpec::default()
            },
        }
    }

    #[test]
    fn dependency_order_places_dependencies_first() {
        let manifest = manifest(vec![
            repo("web", &["shared"]),
            repo("shared", &[]),
            repo("api", &["shared"]),
        ]);
        assert_eq!(
            manifest.dependency_order().unwrap(),
            vec!["shared", "web", "api"]
        );
    }

    #[test]
    fn dependency_cycle_is_rejected() {
        let manifest = manifest(vec![repo("api", &["web"]), repo("web", &["api"])]);
        let error = manifest.validate().unwrap_err();
        assert_eq!(error.code, "project_dependency_cycle");
        assert!(error.message.contains("api") && error.message.contains("web"));
    }

    #[test]
    fn path_escape_and_secret_environment_are_rejected() {
        let mut manifest = manifest(vec![ProjectRepo {
            id: "api".into(),
            path: PathBuf::from("../api"),
            base: None,
            depends_on: Vec::new(),
        }]);
        manifest.setup.push(ProjectCommand {
            id: "prepare".into(),
            repository: Some("api".into()),
            cwd: None,
            argv: vec!["just".into(), "setup".into()],
            environment: BTreeMap::from([("API_TOKEN".into(), "not-allowed".into())]),
        });

        let error = manifest.validate().unwrap_err();
        assert!(error.message.contains("may not escape"));
        assert!(error.message.contains("looks secret"));
    }

    #[test]
    fn commands_cannot_override_task_runtime_environment() {
        let mut manifest = manifest(vec![repo("api", &[])]);
        manifest.setup.push(ProjectCommand {
            id: "prepare".into(),
            repository: Some("api".into()),
            cwd: None,
            argv: vec!["just".into(), "setup".into()],
            environment: BTreeMap::from([
                ("TMPDIR".into(), "/shared/tmp".into()),
                ("COMPOSE_PROJECT_NAME".into(), "shared".into()),
                ("COMPOSE_FILE".into(), "/shared/compose.yaml".into()),
                ("GOWILD_TASK_ROOT".into(), "/shared/task".into()),
            ]),
        });

        let error = manifest.validate().unwrap_err();

        assert!(error.message.contains("TMPDIR"));
        assert!(error.message.contains("COMPOSE_PROJECT_NAME"));
        assert!(error.message.contains("COMPOSE_FILE"));
        assert!(error.message.contains("GOWILD_TASK_ROOT"));
        assert!(error.message.contains("may not override"));
    }

    #[test]
    fn trust_is_required_only_for_executable_content() {
        let mut manifest = manifest(vec![repo("api", &[])]);
        assert!(!manifest.requires_trust());
        manifest.tests.push(ProjectCommand {
            id: "test-api".into(),
            repository: Some("api".into()),
            cwd: None,
            argv: vec!["cargo".into(), "test".into()],
            environment: BTreeMap::new(),
        });
        assert!(manifest.requires_trust());
    }

    #[test]
    fn compose_requires_one_direct_detached_task_scoped_stack() {
        let valid_commands = [
            vec!["docker", "compose", "-f", "compose.yml", "up", "--detach"],
            vec!["/usr/local/bin/docker-compose", "up", "-d"],
            vec!["C:\\Docker\\docker.exe", "compose", "up", "-d"],
        ];
        for argv in valid_commands {
            let mut candidate = manifest(vec![repo("api", &[])]);
            candidate.services = vec![compose_service("stack", &argv)];
            candidate.validate().unwrap();
        }

        let invalid_commands = [
            vec!["sh", "-c", "docker compose up -d"],
            vec!["docker", "compose", "up"],
            vec!["docker", "compose", "--project-name", "shared", "up", "-d"],
            vec!["docker", "compose", "-pother", "up", "-d"],
            vec![
                "docker",
                "compose",
                "--project-directory=../shared",
                "up",
                "-d",
            ],
        ];
        for argv in invalid_commands {
            let mut candidate = manifest(vec![repo("api", &[])]);
            candidate.services = vec![compose_service("stack", &argv)];
            let error = candidate.validate().unwrap_err();
            assert!(error.message.contains("Compose service 'stack'"));
        }

        let mut duplicated = manifest(vec![repo("api", &[])]);
        duplicated.services = vec![
            compose_service("api-stack", &["docker", "compose", "up", "-d"]),
            compose_service("web-stack", &["docker-compose", "up", "-d"]),
        ];
        assert!(duplicated
            .validate()
            .unwrap_err()
            .message
            .contains("at most one Compose service"));
    }
}
