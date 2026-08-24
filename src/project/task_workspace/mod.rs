#![allow(
    dead_code,
    reason = "task lifecycle consumers land in the next stacked change"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::manifest::LoadedProject;
use super::model::ProjectError;

pub(crate) mod branch;
pub(crate) mod cleanup;
mod cleanup_safety;
pub(crate) mod provision;
pub(crate) mod repository;
mod rules;
pub(crate) mod runtime_commands;
mod runtime_layout;
mod runtime_ports;
mod runtime_validation;
mod validation;

use rules::{
    phase_transition_allowed, resources_conflict, runtime_namespace, validate_absolute_clean_path,
    validate_identifier,
};
pub use runtime_ports::TaskPortBroker;

#[cfg(test)]
mod branch_tests;
#[cfg(test)]
mod cleanup_tests;
#[cfg(test)]
mod provision_tests;
#[cfg(test)]
mod repository_tests;
#[cfg(test)]
mod runtime_commands_tests;
#[cfg(test)]
mod runtime_layout_tests;
#[cfg(test)]
mod runtime_ports_tests;
#[cfg(test)]
mod tests;

pub const TASK_WORKSPACE_VERSION: u32 = 1;
const MAX_TRANSITIONS: usize = 10_000;

/// Persisted state for one isolated AI task across every repository in a project.
///
/// This is an ownership manifest, not a cache. Mutation code must persist a
/// planned transition before changing external state and persist its terminal
/// state afterwards.
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
    pub phase: TaskWorkspacePhase,
    pub repositories: BTreeMap<String, TaskRepository>,
    pub runtime: RuntimeIsolation,
    pub journal: Vec<TaskTransition>,
    pub revision: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskWorkspacePhase {
    Planned,
    Provisioning,
    Ready,
    Running,
    Stopped,
    Cleaning,
    NeedsAttention,
    Cleaned,
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
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub declared_containers: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub declared_databases: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub declared_data: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub declared_caches: BTreeSet<String>,
    pub compose_enabled: bool,
    #[serde(default)]
    pub ports: BTreeMap<String, u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTransition {
    pub sequence: u64,
    pub operation: TaskTransitionOperation,
    pub resource: OwnedResource,
    pub state: TaskTransitionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskTransitionOperation {
    Acquire,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskTransitionState {
    Planned,
    Applied,
    Failed,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnedResource {
    WorkspaceDirectory {
        path: PathBuf,
    },
    RuntimeDirectory {
        path: PathBuf,
    },
    RepositoryWorktree {
        repository_id: String,
        source_path: PathBuf,
        checkout_path: PathBuf,
        base_commit: String,
    },
    RepositoryBranch {
        repository_id: String,
        checkout_path: PathBuf,
        branch: String,
        base_commit: String,
    },
    PortReservation {
        name: String,
        port: u16,
    },
    ComposeProject {
        name: String,
    },
    ServiceProcess {
        service_id: String,
        instance_id: String,
    },
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
        let runtime = RuntimeIsolation::for_task(
            &project.manifest,
            namespace,
            &root,
            &project.manifest.id,
            &id,
        );
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
            phase: TaskWorkspacePhase::Planned,
            repositories,
            runtime,
            journal: Vec::new(),
            revision: 0,
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

    pub fn transition_phase(&mut self, next: TaskWorkspacePhase) -> Result<(), ProjectError> {
        if !phase_transition_allowed(self.phase, next) {
            return Err(ProjectError::new(
                "invalid_task_workspace_phase_transition",
                format!(
                    "task workspace cannot move from {:?} to {:?}",
                    self.phase, next
                ),
            ));
        }
        self.phase = next;
        self.bump_revision()?;
        Ok(())
    }

    pub fn plan_transition(
        &mut self,
        operation: TaskTransitionOperation,
        resource: OwnedResource,
    ) -> Result<u64, ProjectError> {
        if self.journal.len() >= MAX_TRANSITIONS {
            return Err(ProjectError::new(
                "task_workspace_journal_full",
                "task workspace transition journal reached its safety limit",
            ));
        }
        self.validate_resource(&resource)?;
        if self.journal.iter().any(|transition| {
            transition.state == TaskTransitionState::Planned
                && resources_conflict(&transition.resource, &resource)
        }) {
            return Err(ProjectError::new(
                "duplicate_task_workspace_transition",
                "a pending transition already reserves that resource",
            ));
        }
        match operation {
            TaskTransitionOperation::Acquire
                if self.journal.iter().any(|transition| {
                    transition.state == TaskTransitionState::Applied
                        && transition.operation == TaskTransitionOperation::Acquire
                        && self.resource_is_owned(&transition.resource)
                        && resources_conflict(&transition.resource, &resource)
                }) =>
            {
                return Err(ProjectError::new(
                    "task_workspace_resource_collision",
                    "an owned task resource conflicts with the requested acquisition",
                ));
            }
            TaskTransitionOperation::Release if !self.resource_is_owned(&resource) => {
                return Err(ProjectError::new(
                    "unowned_task_workspace_resource",
                    "cleanup cannot release a resource this task does not own",
                ));
            }
            _ => {}
        }
        self.validate_operation_precondition(operation, &resource)?;
        let sequence = self
            .journal
            .last()
            .map(|transition| transition.sequence.checked_add(1))
            .unwrap_or(Some(1))
            .ok_or_else(|| {
                ProjectError::new(
                    "task_workspace_sequence_exhausted",
                    "task workspace transition sequence is exhausted",
                )
            })?;
        self.journal.push(TaskTransition {
            sequence,
            operation,
            resource,
            state: TaskTransitionState::Planned,
            failure_code: None,
        });
        self.bump_revision()?;
        Ok(sequence)
    }

    pub fn finish_transition(
        &mut self,
        sequence: u64,
        state: TaskTransitionState,
        failure_code: Option<&str>,
    ) -> Result<(), ProjectError> {
        if state == TaskTransitionState::Planned {
            return Err(ProjectError::new(
                "invalid_task_workspace_transition_state",
                "a planned transition must finish as applied, failed, or rolled back",
            ));
        }
        let index = self
            .journal
            .iter()
            .position(|transition| transition.sequence == sequence)
            .ok_or_else(|| {
                ProjectError::new(
                    "unknown_task_workspace_transition",
                    format!("task workspace transition {sequence} does not exist"),
                )
            })?;
        if self.journal[index].state != TaskTransitionState::Planned {
            return Err(ProjectError::new(
                "task_workspace_transition_already_finished",
                format!("task workspace transition {sequence} is already terminal"),
            ));
        }
        let failure_code = failure_code.map(str::to_string);
        if state == TaskTransitionState::Failed {
            let code = failure_code.as_deref().ok_or_else(|| {
                ProjectError::new(
                    "missing_task_workspace_failure_code",
                    "a failed transition requires a stable failure code",
                )
            })?;
            validate_identifier("transition failure code", code)?;
        } else if failure_code.is_some() {
            return Err(ProjectError::new(
                "unexpected_task_workspace_failure_code",
                "only failed transitions may record a failure code",
            ));
        }
        if state == TaskTransitionState::Applied {
            let transition = self.journal[index].clone();
            self.apply_transition_to_state(&transition)?;
        }
        self.journal[index].state = state;
        self.journal[index].failure_code = failure_code;
        self.bump_revision()?;
        Ok(())
    }
}
