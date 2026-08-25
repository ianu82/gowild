use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const PROJECT_TASK_API_VERSION: u32 = 1;
pub const DEFAULT_PROJECT_TASK_PAGE_SIZE: u16 = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskListParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskGetParams {
    pub path: String,
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskCreateParams {
    pub path: String,
    pub task_id: String,
    pub outcome: String,
    pub agent: ProjectTaskAgent,
    pub route: ProjectTaskRouteInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskLifecycleParams {
    pub path: String,
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskOperationParams {
    pub operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskOperationInfo {
    pub operation_id: String,
    pub project_id: String,
    pub project_root: String,
    pub task_id: String,
    pub kind: ProjectTaskOperationKind,
    pub status: ProjectTaskOperationStatus,
    pub cancellation_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProjectTaskOperationProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProjectTaskOperationError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTaskOperationKind {
    Provision,
    Cleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTaskOperationStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskOperationProgress {
    pub stage: ProjectTaskOperationStage,
    pub completed_steps: usize,
    pub total_steps: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum ProjectTaskOperationStage {
    Validating,
    WorkspaceRoot,
    RuntimeLayout,
    Repository { repository_id: String },
    RepositoryBranch { repository_id: String },
    RepositoryWorktree { repository_id: String },
    Port { name: String },
    RuntimeDirectory { path: String },
    Finalizing,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskOperationError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskProjectInfo {
    pub project_id: String,
    pub name: String,
    pub root: String,
    pub manifest_digest: String,
    pub trust: ProjectTaskTrust,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTaskTrust {
    NotRequired,
    Trusted,
    Untrusted,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskSummary {
    pub task_id: String,
    pub project_id: String,
    pub outcome: String,
    pub agent: ProjectTaskAgent,
    pub route: ProjectTaskRouteInfo,
    pub phase: ProjectTaskPhase,
    pub revision: u64,
    pub repository_count: usize,
    pub active_repository_count: usize,
    pub current_project: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_set: Option<ProjectTaskChangeSetSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskInfo {
    #[serde(flatten)]
    pub summary: ProjectTaskSummary,
    pub task_schema_version: u32,
    pub manifest_digest: String,
    pub root: String,
    pub repositories: Vec<ProjectTaskRepositoryInfo>,
    pub isolation: ProjectTaskIsolationInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTaskAgent {
    Codex,
    Claude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTaskProtocol {
    OpenAiResponses,
    AnthropicMessages,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskRouteInfo {
    pub gateway_id: String,
    pub protocol: ProjectTaskProtocol,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTaskPhase {
    Planned,
    Provisioning,
    Ready,
    Running,
    Stopped,
    Cleaning,
    NeedsAttention,
    Cleaned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskRepositoryInfo {
    pub repository_id: String,
    pub source_path: String,
    pub base_commit: String,
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Secret-safe runtime ownership facts. Only environment key names are exposed;
/// values never cross the API boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskIsolationInfo {
    pub namespace: String,
    pub root: String,
    pub temp: String,
    pub cache: String,
    pub data: String,
    pub compose_project: String,
    pub compose_enabled: bool,
    pub environment_keys: Vec<String>,
    pub declared_services: Vec<String>,
    pub declared_ports: Vec<String>,
    pub declared_containers: Vec<String>,
    pub declared_databases: Vec<String>,
    pub declared_data: Vec<String>,
    pub declared_caches: Vec<String>,
    pub ports: BTreeMap<String, u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectTaskChangeSetSummary {
    pub record_revision: u64,
    pub task_revision: u64,
    pub stale: bool,
    pub repository_count: usize,
    pub affected_repository_count: usize,
    pub checks: ProjectTaskCheckSummary,
    pub planned_pull_request_count: usize,
    pub draft_pull_request_count: usize,
    pub merge_gate: ProjectTaskMergeGate,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct ProjectTaskCheckSummary {
    pub pending: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectTaskMergeGate {
    AwaitingApproval,
    ApprovedByHuman,
    ApprovedByPolicy,
}
