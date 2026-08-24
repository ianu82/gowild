#![allow(
    dead_code,
    reason = "task lifecycle consumers land in the next stacked change"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::manifest::LoadedProject;
use super::model::ProjectError;

mod rules;
mod runtime_validation;
mod validation;

use rules::{runtime_namespace, validate_absolute_clean_path};

#[cfg(test)]
mod tests;

pub const TASK_WORKSPACE_VERSION: u32 = 1;

/// Persisted state for one isolated AI task across every repository in a project.
///
/// This is an ownership boundary, not a cache. Lifecycle code may only create
/// resources represented by this immutable task definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskWorkspace {
    pub schema_version: u32,
    pub id: String,
    pub project_id: String,
    pub manifest_digest: String,
    pub outcome: String,
    pub agent: TaskAgent,
    pub route: TaskRoute,
    pub root: PathBuf,
    pub repositories: BTreeMap<String, TaskRepository>,
    pub runtime: RuntimeIsolation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAgent {
    Codex,
    Claude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskProtocol {
    OpenAiResponses,
    AnthropicMessages,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRoute {
    pub gateway_id: String,
    pub protocol: TaskProtocol,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRepository {
    pub source_path: PathBuf,
    pub base_commit: String,
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<TaskWorktree>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskWorktree {
    pub checkout_path: PathBuf,
    pub head_commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeIsolation {
    pub namespace: String,
    pub root: PathBuf,
    pub temp: PathBuf,
    pub cache: PathBuf,
    pub data: PathBuf,
    pub compose_project: String,
    pub environment: BTreeMap<String, String>,
    pub declared_services: BTreeSet<String>,
    pub declared_ports: BTreeSet<String>,
    pub compose_enabled: bool,
    #[serde(default)]
    pub ports: BTreeMap<String, u16>,
}

impl TaskWorkspace {
    pub fn new(
        project: &LoadedProject,
        id: impl Into<String>,
        outcome: impl Into<String>,
        agent: TaskAgent,
        route: TaskRoute,
        task_store_root: PathBuf,
    ) -> Result<Self, ProjectError> {
        let id = id.into();
        let namespace = runtime_namespace(&project.manifest.id, &id, &project.digest);
        validate_absolute_clean_path("task workspace store root", &task_store_root)?;
        if task_store_root.parent().is_none() {
            return Err(ProjectError::new(
                "invalid_task_workspace_path",
                "task workspace store root cannot be the filesystem root",
            ));
        }
        let root = task_store_root.join(&namespace);
        let runtime_root = root.join("runtime");
        let declared_services = project
            .manifest
            .services
            .iter()
            .map(|service| service.id.clone())
            .collect();
        let declared_ports = project
            .manifest
            .services
            .iter()
            .flat_map(|service| {
                service
                    .isolation
                    .ports
                    .iter()
                    .map(|port| format!("{}.{port}", service.id))
            })
            .collect();
        let runtime = RuntimeIsolation {
            namespace: namespace.clone(),
            root: runtime_root.clone(),
            temp: runtime_root.join("tmp"),
            cache: runtime_root.join("cache"),
            data: runtime_root.join("data"),
            compose_project: namespace.clone(),
            environment: BTreeMap::from([
                ("COMPOSE_PROJECT_NAME".into(), namespace),
                ("GOWILD_PROJECT_ID".into(), project.manifest.id.clone()),
                ("GOWILD_TASK_ID".into(), id.clone()),
                ("GOWILD_TASK_ROOT".into(), root.display().to_string()),
                (
                    "GOWILD_RUNTIME_ROOT".into(),
                    runtime_root.display().to_string(),
                ),
            ]),
            declared_services,
            declared_ports,
            compose_enabled: project
                .manifest
                .services
                .iter()
                .any(|service| service.isolation.compose),
            ports: BTreeMap::new(),
        };
        let repositories = project
            .repositories
            .iter()
            .map(|repository| {
                (
                    repository.id.clone(),
                    TaskRepository {
                        source_path: repository.path.clone(),
                        base_commit: repository.base_commit.clone(),
                        depends_on: repository.depends_on.clone(),
                        worktree: None,
                    },
                )
            })
            .collect();
        let workspace = Self {
            schema_version: TASK_WORKSPACE_VERSION,
            id,
            project_id: project.manifest.id.clone(),
            manifest_digest: project.digest.clone(),
            outcome: outcome.into(),
            agent,
            route,
            root,
            repositories,
            runtime,
        };
        workspace.validate(project)?;
        Ok(workspace)
    }

    pub fn repository_checkout_path(&self, repository_id: &str) -> PathBuf {
        self.root.join("repositories").join(repository_id)
    }

    pub fn branch_name(&self, repository_id: &str) -> String {
        format!("gowild/{}/{repository_id}", self.id)
    }
}
