use std::path::Path;

use super::manifest::{load_project_definition, resolve_project_definition, LoadedProject};
use super::private_state::{ProjectPrivateState, ProjectPrivateStateRepository};
use super::task_workspace::repository::TaskWorkspaceRepository;
use super::{ProjectDefinition, ProjectError};

/// One validated view of the checked-in definition, machine-private overrides,
/// resolved repositories and GoWild-owned task state roots.
#[derive(Debug)]
pub(super) struct ProjectTaskContext {
    pub definition: ProjectDefinition,
    pub private_state: ProjectPrivateState,
    pub project: LoadedProject,
    pub states: TaskWorkspaceRepository,
}

impl ProjectTaskContext {
    pub fn open(path: &Path) -> Result<Self, ProjectError> {
        let definition = load_project_definition(path)?;
        let private_state_repository = ProjectPrivateStateRepository::in_default_state_dir();
        let private_state = private_state_repository.load(&definition)?;
        let project = resolve_project_definition(definition.clone(), &private_state.overrides)?;
        let states = TaskWorkspaceRepository::in_default_state_dir(&definition);
        Ok(Self {
            definition,
            private_state,
            project,
            states,
        })
    }
}
