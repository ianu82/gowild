use std::path::Path;

use super::task_context::ProjectTaskContext;
use super::task_workspace::{validate_outcome, TaskAgent, TaskRoute, TaskWorkspace};
use super::{ProjectError, ProjectTaskReader};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateProjectTask {
    pub task_id: String,
    pub outcome: String,
    pub agent: TaskAgent,
    pub route: TaskRoute,
}

impl CreateProjectTask {
    pub fn validate(&self) -> Result<(), ProjectError> {
        ProjectTaskReader::validate_task_id(&self.task_id)?;
        validate_outcome(&self.outcome)?;
        self.route.validate(self.agent)
    }
}

/// Mutation boundary for durable project-task state. Creating a task records
/// intent only; external repositories and runtime resources remain untouched.
#[derive(Debug)]
pub struct ProjectTaskService {
    context: ProjectTaskContext,
}

impl ProjectTaskService {
    pub fn open(path: &Path) -> Result<Self, ProjectError> {
        Ok(Self {
            context: ProjectTaskContext::open(path)?,
        })
    }

    pub fn create(&self, request: CreateProjectTask) -> Result<TaskWorkspace, ProjectError> {
        request.validate()?;
        let task = TaskWorkspace::new(
            &self.context.project,
            request.task_id,
            request.outcome,
            request.agent,
            request.route,
            self.context.states.workspace_store_root().to_path_buf(),
        )?;
        self.context.states.create(&task)?;
        Ok(task)
    }

    pub fn reader(&self) -> ProjectTaskReader {
        ProjectTaskReader::from_context(self.context.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::task_workspace::provision_tests::ProjectFixture;
    use crate::project::task_workspace::{TaskProtocol, TaskWorkspacePhase};

    struct StateHomeGuard(Option<std::ffi::OsString>);

    impl Drop for StateHomeGuard {
        fn drop(&mut self) {
            if let Some(value) = self.0.take() {
                std::env::set_var("XDG_STATE_HOME", value);
            } else {
                std::env::remove_var("XDG_STATE_HOME");
            }
        }
    }

    #[test]
    fn create_persists_planned_intent_without_materializing_external_resources() {
        let _lock = crate::config::test_config_env_lock().lock().unwrap();
        let previous_state_home = std::env::var_os("XDG_STATE_HOME");
        let _state_home_guard = StateHomeGuard(previous_state_home);
        let fixture = ProjectFixture::new(true);
        let state_home = fixture.root.join("service-state-home");
        std::env::set_var("XDG_STATE_HOME", &state_home);
        std::fs::write(
            &fixture.definition.manifest_path,
            crate::project::render_manifest(&fixture.definition.manifest).unwrap(),
        )
        .unwrap();
        let service = ProjectTaskService::open(&fixture.root).unwrap();
        let request = request("create-service");

        let created = service.create(request.clone()).unwrap();
        let retried = service.create(request).unwrap();

        assert_eq!(created, retried);
        assert_eq!(created.phase, TaskWorkspacePhase::Planned);
        assert_eq!(created.revision, 0);
        assert!(created.journal.is_empty());
        assert!(!created.root.exists());
        assert!(created
            .repositories
            .values()
            .all(|repository| repository.worktree.is_none()));
        assert!(crate::config::state_dir().join("project-tasks").exists());
    }

    #[test]
    fn conflicting_create_keeps_the_original_task_unchanged() {
        let _lock = crate::config::test_config_env_lock().lock().unwrap();
        let previous_state_home = std::env::var_os("XDG_STATE_HOME");
        let _state_home_guard = StateHomeGuard(previous_state_home);
        let fixture = ProjectFixture::new(false);
        std::env::set_var("XDG_STATE_HOME", fixture.root.join("conflict-state-home"));
        std::fs::write(
            &fixture.definition.manifest_path,
            crate::project::render_manifest(&fixture.definition.manifest).unwrap(),
        )
        .unwrap();
        let service = ProjectTaskService::open(&fixture.root).unwrap();
        let original = service.create(request("conflicting-create")).unwrap();
        let mut conflicting = request("conflicting-create");
        conflicting.outcome = "A different outcome".into();

        let error = service.create(conflicting).unwrap_err();
        assert_eq!(error.code, "task_workspace_already_exists");
        let reader = crate::project::ProjectTaskReader::open(&fixture.root).unwrap();
        assert_eq!(reader.get("conflicting-create").unwrap().task, original);
    }

    fn request(task_id: &str) -> CreateProjectTask {
        CreateProjectTask {
            task_id: task_id.into(),
            outcome: "Coordinate one change across every repository".into(),
            agent: TaskAgent::Claude,
            route: TaskRoute {
                gateway_id: "mindshub".into(),
                protocol: TaskProtocol::AnthropicMessages,
                model: "provider/team/model".into(),
            },
        }
    }
}
