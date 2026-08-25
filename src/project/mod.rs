pub(crate) mod change_set;
mod discovery;
mod manifest;
mod model;
mod overrides;
mod private_state;
mod task_context;
mod task_read;
mod task_service;
pub(crate) mod task_workspace;

pub(crate) use discovery::{discover_project, DiscoveryOptions};
pub(crate) use manifest::{
    load_project_definition, render_manifest, resolve_project_definition, ProjectDefinition,
};
pub(crate) use model::{ProjectError, PROJECT_MANIFEST_FILE};
pub(crate) use private_state::ProjectTrustStatus;
pub(crate) use private_state::{ProjectPrivateState, ProjectPrivateStateRepository};
pub(crate) use task_read::{ProjectTaskReader, ProjectTaskSnapshot};
#[allow(
    unused_imports,
    reason = "project task mutation API consumer lands in the next stacked change"
)]
pub(crate) use task_service::{CreateProjectTask, ProjectTaskService};
