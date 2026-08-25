use std::path::Path;

use super::change_set::ChangeSet;
use super::private_state::ProjectTrustStatus;
use super::task_context::ProjectTaskContext;
use super::task_workspace::{validate_identifier, TaskWorkspace};
use super::ProjectError;

const MAX_TASK_READ_PAGE_SIZE: usize = 200;

/// A validated, read-only view of the durable state for one project task.
///
/// Stale tasks remain visible so the UI can explain and recover them. The
/// `current_project` flag is false when the task's immutable project binding no
/// longer matches the current manifest or private overrides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTaskSnapshot {
    pub task: TaskWorkspace,
    pub current_project: bool,
    pub attention_code: Option<&'static str>,
    pub change_set_revision: Option<u64>,
    pub change_set: Option<ChangeSet>,
    pub change_set_stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTaskPage {
    pub tasks: Vec<ProjectTaskSnapshot>,
    pub next_after: Option<String>,
}

/// Loads bounded task facts through the same manifest, override and durable
/// state boundaries used by lifecycle operations.
#[derive(Debug)]
pub struct ProjectTaskReader {
    context: ProjectTaskContext,
}

impl ProjectTaskReader {
    pub fn open(path: &Path) -> Result<Self, ProjectError> {
        Ok(Self {
            context: ProjectTaskContext::open(path)?,
        })
    }

    pub fn project_id(&self) -> &str {
        &self.context.project.manifest.id
    }

    pub fn project_name(&self) -> &str {
        &self.context.project.manifest.name
    }

    pub fn project_root(&self) -> &Path {
        &self.context.project.root
    }

    pub fn manifest_digest(&self) -> &str {
        &self.context.project.digest
    }

    pub fn trust_status(&self) -> ProjectTrustStatus {
        self.context
            .private_state
            .trust_status(&self.context.definition)
    }

    pub fn list_page(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ProjectTaskPage, ProjectError> {
        Self::validate_page(after, limit)?;
        let task_ids = self.context.states.list_ids()?;
        let start = after.map_or(0, |after| {
            task_ids.partition_point(|task_id| task_id.as_str() <= after)
        });
        let end = start.saturating_add(limit).min(task_ids.len());
        let tasks = task_ids[start..end]
            .iter()
            .map(|task_id| self.get(task_id))
            .collect::<Result<Vec<_>, _>>()?;
        let next_after = if end < task_ids.len() {
            tasks.last().map(|snapshot| snapshot.task.id.clone())
        } else {
            None
        };
        Ok(ProjectTaskPage { tasks, next_after })
    }

    pub fn validate_page(after: Option<&str>, limit: usize) -> Result<(), ProjectError> {
        if limit == 0 || limit > MAX_TASK_READ_PAGE_SIZE {
            return Err(ProjectError::new(
                "invalid_project_task_page_size",
                format!("project task page size must be between 1 and {MAX_TASK_READ_PAGE_SIZE}"),
            ));
        }
        if let Some(after) = after {
            validate_identifier("project task cursor", after).map_err(|_| {
                ProjectError::new(
                    "invalid_project_task_cursor",
                    "project task cursor is not a safe task identifier",
                )
            })?;
        }
        Ok(())
    }

    pub fn get(&self, task_id: &str) -> Result<ProjectTaskSnapshot, ProjectError> {
        Self::validate_task_id(task_id)?;
        let task = self.context.states.load(task_id)?;
        let project_validation = task.validate(&self.context.project);
        let (current_project, attention_code) = match project_validation {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error.code)),
        };
        let change_set_record = self.context.states.load_change_set(&task)?;
        let change_set_revision = change_set_record.as_ref().map(|record| record.revision);
        let change_set = change_set_record.map(|record| record.change_set);
        let change_set_stale = change_set
            .as_ref()
            .is_some_and(|change_set| change_set.is_stale_for_task(&task));
        Ok(ProjectTaskSnapshot {
            task,
            current_project,
            attention_code,
            change_set_revision,
            change_set,
            change_set_stale,
        })
    }

    pub fn validate_task_id(task_id: &str) -> Result<(), ProjectError> {
        validate_identifier("project task id", task_id).map_err(|_| {
            ProjectError::new(
                "invalid_project_task_id",
                "project task id is not a safe task identifier",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::resolve_project_definition;
    use crate::project::task_workspace::provision_tests::ProjectFixture;
    use crate::project::task_workspace::repository::TaskWorkspaceRepository;
    use crate::project::ProjectPrivateStateRepository;

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
    fn open_uses_the_current_manifest_overrides_and_default_task_store() {
        let _lock = crate::config::test_config_env_lock().lock().unwrap();
        let previous_state_home = std::env::var_os("XDG_STATE_HOME");
        let _state_home_guard = StateHomeGuard(previous_state_home);
        let fixture = ProjectFixture::new(false);
        let state_home = fixture.root.join("default-state-home");
        std::env::set_var("XDG_STATE_HOME", &state_home);
        std::fs::write(
            &fixture.definition.manifest_path,
            crate::project::render_manifest(&fixture.definition.manifest).unwrap(),
        )
        .unwrap();
        let definition = crate::project::load_project_definition(&fixture.root).unwrap();
        let private_state = ProjectPrivateStateRepository::in_default_state_dir()
            .load(&definition)
            .unwrap();
        let project =
            resolve_project_definition(definition.clone(), &private_state.overrides).unwrap();
        let states = TaskWorkspaceRepository::in_default_state_dir(&definition);
        let task = TaskWorkspace::new(
            &project,
            "reader-open",
            "Read durable task facts",
            crate::project::task_workspace::TaskAgent::Claude,
            crate::project::task_workspace::TaskRoute {
                gateway_id: "mindshub".into(),
                protocol: crate::project::task_workspace::TaskProtocol::AnthropicMessages,
                model: "test-model".into(),
            },
            states.workspace_store_root().to_path_buf(),
        )
        .unwrap();
        states.create(&task).unwrap();

        let reader = ProjectTaskReader::open(&fixture.root).unwrap();
        let snapshot = reader.get("reader-open").unwrap();
        assert_eq!(reader.project_id(), project.manifest.id);
        assert!(snapshot.current_project);
        assert_eq!(snapshot.task, task);
    }

    #[test]
    fn reader_lists_validated_tasks_in_stable_order() {
        let fixture = ProjectFixture::new(false);
        let states = fixture.states.clone();
        let project = fixture.project.clone();
        for task_id in ["second", "first"] {
            let task = TaskWorkspace::new(
                &project,
                task_id,
                format!("Outcome for {task_id}"),
                crate::project::task_workspace::TaskAgent::Codex,
                crate::project::task_workspace::TaskRoute {
                    gateway_id: "mindshub".into(),
                    protocol: crate::project::task_workspace::TaskProtocol::OpenAiResponses,
                    model: "test-model".into(),
                },
                states.workspace_store_root().to_path_buf(),
            )
            .unwrap();
            states.create(&task).unwrap();
        }

        let reader = reader_for(&fixture, project, states);
        let page = reader.list_page(None, 100).unwrap();
        let tasks = page.tasks;
        assert_eq!(
            tasks
                .iter()
                .map(|snapshot| snapshot.task.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(tasks.iter().all(|snapshot| snapshot.current_project));
        assert!(tasks.iter().all(|snapshot| snapshot.change_set.is_none()));
        assert_eq!(page.next_after, None);
    }

    #[test]
    fn reader_pages_tasks_with_a_stable_validated_cursor() {
        let fixture = ProjectFixture::new(false);
        for task_id in ["third", "first", "second"] {
            fixture.create_task(task_id);
        }
        let reader = reader_for(&fixture, fixture.project.clone(), fixture.states.clone());

        let first = reader.list_page(None, 2).unwrap();
        assert_eq!(
            first
                .tasks
                .iter()
                .map(|snapshot| snapshot.task.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(first.next_after.as_deref(), Some("second"));
        let second = reader.list_page(first.next_after.as_deref(), 2).unwrap();
        assert_eq!(second.tasks[0].task.id, "third");
        assert_eq!(second.next_after, None);

        assert_eq!(
            reader.list_page(Some("../escape"), 2).unwrap_err().code,
            "invalid_project_task_cursor"
        );
        assert_eq!(
            reader.list_page(None, 0).unwrap_err().code,
            "invalid_project_task_page_size"
        );
    }

    #[test]
    fn reader_keeps_a_stale_task_visible_with_structured_attention() {
        let fixture = ProjectFixture::new(false);
        fixture.create_task("stale-task");
        let mut current_project = fixture.project.clone();
        current_project.digest = "f".repeat(64);

        let reader = reader_for(&fixture, current_project, fixture.states.clone());
        let snapshot = reader.get("stale-task").unwrap();
        assert!(!snapshot.current_project);
        assert_eq!(
            snapshot.attention_code,
            Some("task_workspace_project_mismatch")
        );
    }

    fn reader_for(
        fixture: &ProjectFixture,
        project: crate::project::manifest::LoadedProject,
        states: TaskWorkspaceRepository,
    ) -> ProjectTaskReader {
        ProjectTaskReader {
            context: ProjectTaskContext {
                definition: fixture.definition.clone(),
                private_state: fixture.private_state.clone(),
                project,
                states,
            },
        }
    }
}
