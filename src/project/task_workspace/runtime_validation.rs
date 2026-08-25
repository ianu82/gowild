use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sha2::{Digest, Sha256};

use super::rules::{validate_identifier, validate_manifest_identifier};
use super::{RuntimeIsolation, TaskAgent, TaskProtocol, TaskRoute};
use crate::project::model::{ProjectError, ProjectManifest};

#[derive(Default)]
struct RuntimeDeclarations {
    services: BTreeSet<String>,
    ports: BTreeSet<String>,
    containers: BTreeSet<String>,
    databases: BTreeSet<String>,
    data: BTreeSet<String>,
    caches: BTreeSet<String>,
    compose_enabled: bool,
}

struct RuntimeEnvironmentContext<'a> {
    namespace: &'a str,
    task_root: &'a Path,
    runtime_root: &'a Path,
    temp: &'a Path,
    cache: &'a Path,
    data: &'a Path,
    project_id: &'a str,
    task_id: &'a str,
}

impl TaskRoute {
    pub(in crate::project) fn validate(&self, agent: TaskAgent) -> Result<(), ProjectError> {
        validate_identifier("gateway id", &self.gateway_id)?;
        if self.model.trim().is_empty()
            || self.model.len() > 512
            || self.model.chars().any(char::is_control)
        {
            return Err(ProjectError::new(
                "invalid_task_workspace_route",
                "task route model must be non-empty and contain no control characters",
            ));
        }
        if !matches!(
            (agent, self.protocol),
            (TaskAgent::Codex, TaskProtocol::OpenAiResponses)
                | (TaskAgent::Claude, TaskProtocol::AnthropicMessages)
        ) {
            return Err(ProjectError::new(
                "invalid_task_workspace_route",
                "task route protocol is incompatible with the selected coding CLI",
            ));
        }
        Ok(())
    }
}

impl RuntimeIsolation {
    pub(super) fn for_task(
        manifest: &ProjectManifest,
        namespace: String,
        task_root: &Path,
        project_id: &str,
        task_id: &str,
    ) -> Self {
        let root = task_root.join("runtime");
        let temp = root.join("tmp");
        let cache = root.join("cache");
        let data = root.join("data");
        let declarations = runtime_declarations(manifest);
        let environment = runtime_environment(
            RuntimeEnvironmentContext {
                namespace: &namespace,
                task_root,
                runtime_root: &root,
                temp: &temp,
                cache: &cache,
                data: &data,
                project_id,
                task_id,
            },
            &declarations,
        );
        Self {
            namespace: namespace.clone(),
            root,
            temp,
            cache,
            data,
            compose_project: namespace,
            environment,
            declared_services: declarations.services,
            declared_ports: declarations.ports,
            declared_containers: declarations.containers,
            declared_databases: declarations.databases,
            declared_data: declarations.data,
            declared_caches: declarations.caches,
            compose_enabled: declarations.compose_enabled,
            ports: BTreeMap::new(),
        }
    }

    /// Complete non-secret environment applied to project commands, services,
    /// and coding agents once the current manifest has passed its trust gate.
    pub fn command_environment(&self) -> BTreeMap<String, String> {
        let mut environment = self.environment.clone();
        for (name, port) in &self.ports {
            environment.insert(runtime_env_key("GOWILD_PORT", name), port.to_string());
        }
        environment
    }

    pub(super) fn validate(
        &self,
        task_root: &Path,
        project_id: &str,
        task_id: &str,
    ) -> Result<(), ProjectError> {
        validate_identifier("runtime namespace", &self.namespace)?;
        let expected_root = task_root.join("runtime");
        if self.root != expected_root
            || self.temp != expected_root.join("tmp")
            || self.cache != expected_root.join("cache")
            || self.data != expected_root.join("data")
            || self.compose_project != self.namespace
        {
            return Err(ProjectError::new(
                "invalid_task_runtime_boundary",
                "runtime paths or Compose project escape the task namespace",
            ));
        }
        let declarations = self.declarations();
        let expected = runtime_environment(
            RuntimeEnvironmentContext {
                namespace: &self.namespace,
                task_root,
                runtime_root: &self.root,
                temp: &self.temp,
                cache: &self.cache,
                data: &self.data,
                project_id,
                task_id,
            },
            &declarations,
        );
        let legacy =
            legacy_runtime_environment(&self.namespace, task_root, &self.root, project_id, task_id);
        if self.environment != expected && self.environment != legacy {
            return Err(ProjectError::new(
                "invalid_task_runtime_environment",
                "runtime environment contains values outside the task namespace",
            ));
        }
        for service_id in &self.declared_services {
            validate_manifest_identifier("service id", service_id)?;
        }
        for qualified_port in &self.declared_ports {
            validate_qualified_resource(
                &self.declared_services,
                "port reservation",
                qualified_port,
            )?;
        }
        for (kind, resources) in [
            ("container", &self.declared_containers),
            ("database", &self.declared_databases),
            ("data", &self.declared_data),
            ("cache", &self.declared_caches),
        ] {
            for resource in resources {
                validate_qualified_resource(&self.declared_services, kind, resource)?;
            }
        }
        for (kind, prefix, resources) in [
            ("port", "GOWILD_PORT", &self.declared_ports),
            ("container", "GOWILD_CONTAINER", &self.declared_containers),
            ("database", "GOWILD_DATABASE", &self.declared_databases),
            ("data", "GOWILD_DATA", &self.declared_data),
            ("cache", "GOWILD_CACHE", &self.declared_caches),
        ] {
            validate_unique_environment_keys(kind, prefix, resources)?;
        }
        let mut ports = BTreeSet::new();
        for (name, port) in &self.ports {
            validate_identifier("port reservation name", name)?;
            if !self.declared_ports.contains(name) || *port == 0 || !ports.insert(port) {
                return Err(ProjectError::new(
                    "task_workspace_port_collision",
                    "runtime port reservations must be non-zero and unique",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn require_current_manifest(
        &self,
        manifest: &ProjectManifest,
        project_id: &str,
        task_id: &str,
    ) -> Result<(), ProjectError> {
        let expected = runtime_declarations(manifest);
        let task_root = self.root.parent().ok_or_else(|| {
            ProjectError::new(
                "invalid_task_runtime_boundary",
                "runtime root does not have a task workspace parent",
            )
        })?;
        let expected_environment = runtime_environment(
            RuntimeEnvironmentContext {
                namespace: &self.namespace,
                task_root,
                runtime_root: &self.root,
                temp: &self.temp,
                cache: &self.cache,
                data: &self.data,
                project_id,
                task_id,
            },
            &expected,
        );
        if self.declared_services == expected.services
            && self.declared_ports == expected.ports
            && self.declared_containers == expected.containers
            && self.declared_databases == expected.databases
            && self.declared_data == expected.data
            && self.declared_caches == expected.caches
            && self.compose_enabled == expected.compose_enabled
            && self.environment == expected_environment
        {
            Ok(())
        } else {
            Err(ProjectError::new(
                "task_workspace_runtime_manifest_mismatch",
                "runtime declarations no longer match the project manifest",
            ))
        }
    }

    fn declarations(&self) -> RuntimeDeclarations {
        RuntimeDeclarations {
            services: self.declared_services.clone(),
            ports: self.declared_ports.clone(),
            containers: self.declared_containers.clone(),
            databases: self.declared_databases.clone(),
            data: self.declared_data.clone(),
            caches: self.declared_caches.clone(),
            compose_enabled: self.compose_enabled,
        }
    }
}

fn runtime_declarations(manifest: &ProjectManifest) -> RuntimeDeclarations {
    let mut declarations = RuntimeDeclarations::default();
    for service in &manifest.services {
        declarations.services.insert(service.id.clone());
        qualify_all(
            &mut declarations.ports,
            &service.id,
            &service.isolation.ports,
        );
        qualify_all(
            &mut declarations.containers,
            &service.id,
            &service.isolation.containers,
        );
        qualify_all(
            &mut declarations.databases,
            &service.id,
            &service.isolation.databases,
        );
        qualify_all(&mut declarations.data, &service.id, &service.isolation.data);
        qualify_all(
            &mut declarations.caches,
            &service.id,
            &service.isolation.caches,
        );
        declarations.compose_enabled |= service.isolation.compose;
    }
    declarations
}

fn qualify_all(target: &mut BTreeSet<String>, service_id: &str, names: &[String]) {
    target.extend(names.iter().map(|name| format!("{service_id}.{name}")));
}

fn runtime_environment(
    context: RuntimeEnvironmentContext<'_>,
    declarations: &RuntimeDeclarations,
) -> BTreeMap<String, String> {
    let mut environment = legacy_runtime_environment(
        context.namespace,
        context.task_root,
        context.runtime_root,
        context.project_id,
        context.task_id,
    );
    for key in ["TMPDIR", "TMP", "TEMP", "GOWILD_TEMP_ROOT"] {
        environment.insert(key.into(), context.temp.display().to_string());
    }
    for key in ["XDG_CACHE_HOME", "GOWILD_CACHE_ROOT"] {
        environment.insert(key.into(), context.cache.display().to_string());
    }
    for key in ["XDG_DATA_HOME", "GOWILD_DATA_ROOT"] {
        environment.insert(key.into(), context.data.display().to_string());
    }
    for resource in &declarations.containers {
        environment.insert(
            runtime_env_key("GOWILD_CONTAINER", resource),
            runtime_resource_name(context.namespace, resource),
        );
    }
    for resource in &declarations.databases {
        insert_resource_path(
            &mut environment,
            "GOWILD_DATABASE",
            context.data,
            "databases",
            resource,
        );
    }
    for resource in &declarations.data {
        insert_resource_path(
            &mut environment,
            "GOWILD_DATA",
            context.data,
            "services",
            resource,
        );
    }
    for resource in &declarations.caches {
        insert_resource_path(
            &mut environment,
            "GOWILD_CACHE",
            context.cache,
            "services",
            resource,
        );
    }
    environment
}

fn insert_resource_path(
    environment: &mut BTreeMap<String, String>,
    prefix: &str,
    root: &Path,
    kind: &str,
    qualified: &str,
) {
    let Some((service, name)) = qualified.split_once('.') else {
        return;
    };
    environment.insert(
        runtime_env_key(prefix, qualified),
        root.join(kind)
            .join(service)
            .join(name)
            .display()
            .to_string(),
    );
}

fn legacy_runtime_environment(
    namespace: &str,
    task_root: &Path,
    runtime_root: &Path,
    project_id: &str,
    task_id: &str,
) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("COMPOSE_PROJECT_NAME".into(), namespace.to_string()),
        ("GOWILD_PROJECT_ID".into(), project_id.to_string()),
        ("GOWILD_TASK_ID".into(), task_id.to_string()),
        ("GOWILD_TASK_ROOT".into(), task_root.display().to_string()),
        (
            "GOWILD_RUNTIME_ROOT".into(),
            runtime_root.display().to_string(),
        ),
    ])
}

fn validate_qualified_resource(
    services: &BTreeSet<String>,
    kind: &str,
    qualified: &str,
) -> Result<(), ProjectError> {
    let Some((service_id, name)) = qualified.split_once('.') else {
        return Err(ProjectError::new(
            "invalid_task_workspace_runtime_resource",
            format!("declared {kind} is not qualified by its service"),
        ));
    };
    validate_manifest_identifier("service id", service_id)?;
    validate_manifest_identifier(kind, name)?;
    if services.contains(service_id) {
        Ok(())
    } else {
        Err(ProjectError::new(
            "unknown_task_workspace_service",
            format!("declared {kind} references unknown service '{service_id}'"),
        ))
    }
}

fn validate_unique_environment_keys(
    kind: &str,
    prefix: &str,
    resources: &BTreeSet<String>,
) -> Result<(), ProjectError> {
    let mut keys = BTreeSet::new();
    for resource in resources {
        if !keys.insert(runtime_env_key(prefix, resource)) {
            return Err(ProjectError::new(
                "task_workspace_runtime_environment_collision",
                format!("declared {kind} resources map to the same environment key"),
            ));
        }
    }
    Ok(())
}

fn runtime_env_key(prefix: &str, qualified: &str) -> String {
    format!(
        "{prefix}_{}",
        qualified
            .chars()
            .map(|character| match character {
                'a'..='z' => character.to_ascii_uppercase(),
                '0'..='9' => character,
                _ => '_',
            })
            .collect::<String>()
    )
}

fn runtime_resource_name(namespace: &str, qualified: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update([0]);
    hasher.update(qualified.as_bytes());
    let digest = hasher.finalize();
    let suffix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let slug = qualified
        .replace('.', "-")
        .chars()
        .take(5)
        .collect::<String>();
    format!("{namespace}-{slug}-{suffix}")
}
