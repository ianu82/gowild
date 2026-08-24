use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::rules::{validate_identifier, validate_manifest_identifier};
use super::{RuntimeIsolation, TaskAgent, TaskProtocol, TaskRoute};
use crate::project::model::{ProjectError, ProjectManifest};

impl TaskRoute {
    pub(super) fn validate(&self, agent: TaskAgent) -> Result<(), ProjectError> {
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
        let expected = BTreeMap::from([
            ("COMPOSE_PROJECT_NAME".to_string(), self.namespace.clone()),
            ("GOWILD_PROJECT_ID".to_string(), project_id.to_string()),
            ("GOWILD_TASK_ID".to_string(), task_id.to_string()),
            (
                "GOWILD_TASK_ROOT".to_string(),
                task_root.display().to_string(),
            ),
            (
                "GOWILD_RUNTIME_ROOT".to_string(),
                self.root.display().to_string(),
            ),
        ]);
        if self.environment != expected {
            return Err(ProjectError::new(
                "invalid_task_runtime_environment",
                "runtime environment contains values outside the task namespace",
            ));
        }
        for service_id in &self.declared_services {
            validate_manifest_identifier("service id", service_id)?;
        }
        for qualified_port in &self.declared_ports {
            let Some((service_id, port_name)) = qualified_port.split_once('.') else {
                return Err(ProjectError::new(
                    "invalid_task_workspace_port",
                    "declared port is not qualified by its service",
                ));
            };
            validate_manifest_identifier("service id", service_id)?;
            validate_manifest_identifier("port reservation name", port_name)?;
            if !self.declared_services.contains(service_id) {
                return Err(ProjectError::new(
                    "unknown_task_workspace_service",
                    format!("declared port references unknown service '{service_id}'"),
                ));
            }
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
    ) -> Result<(), ProjectError> {
        let expected_services = manifest
            .services
            .iter()
            .map(|service| service.id.clone())
            .collect::<BTreeSet<_>>();
        let expected_ports = manifest
            .services
            .iter()
            .flat_map(|service| {
                service
                    .isolation
                    .ports
                    .iter()
                    .map(|port| format!("{}.{port}", service.id))
            })
            .collect::<BTreeSet<_>>();
        let expected_compose = manifest
            .services
            .iter()
            .any(|service| service.isolation.compose);
        if self.declared_services == expected_services
            && self.declared_ports == expected_ports
            && self.compose_enabled == expected_compose
        {
            Ok(())
        } else {
            Err(ProjectError::new(
                "task_workspace_runtime_manifest_mismatch",
                "runtime declarations no longer match the project manifest",
            ))
        }
    }
}
