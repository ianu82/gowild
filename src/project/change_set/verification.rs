use std::time::Instant;

use super::collector::inspect_task;
use super::{ChangeSet, ChangeSetCheck, CheckStatus};
use crate::project::manifest::LoadedProject;
use crate::project::task_workspace::provision::{
    require_matching_definition, verify_provisioned_task, TaskWorkspaceProvisioner,
};
use crate::project::task_workspace::runtime_commands::{run_invocation, TaskCommandKind};
use crate::project::task_workspace::TaskWorkspacePhase;
use crate::project::{ProjectDefinition, ProjectError, ProjectPrivateState};

impl TaskWorkspaceProvisioner<'_> {
    /// Runs every trusted, declared project test and then captures the resulting
    /// repository facts. Command output remains ephemeral and is deliberately
    /// absent from the returned change set.
    pub fn verify_change_set(
        &self,
        definition: &ProjectDefinition,
        private_state: &ProjectPrivateState,
        project: &LoadedProject,
        task_id: &str,
    ) -> Result<ChangeSet, ProjectError> {
        require_matching_definition(definition, project)?;
        private_state.require_execution_trust(definition)?;
        let _operation_lock = self.states().lock_task_operations(task_id)?;
        let task = self.states().load(task_id)?;
        task.validate(project)?;
        if !matches!(
            task.phase,
            TaskWorkspacePhase::Ready | TaskWorkspacePhase::Stopped
        ) {
            return Err(ProjectError::new(
                "task_change_set_not_verifiable",
                "a change set can run tests only while its task is ready or stopped",
            ));
        }
        verify_provisioned_task(&task)?;
        self.verify_runtime_ports(&task)?;

        let mut checks = std::collections::BTreeMap::new();
        for command in &project.manifest.tests {
            let started = Instant::now();
            let result = self
                .resolve_command(project, &task, TaskCommandKind::Test, &command.id)
                .and_then(run_invocation);
            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let (status, exit_code, failure_code) = match result {
                Ok(result) => (
                    if result.success {
                        CheckStatus::Passed
                    } else {
                        CheckStatus::Failed
                    },
                    result.exit_code,
                    None,
                ),
                Err(error) => (CheckStatus::Failed, None, Some(error.code.to_string())),
            };
            checks.insert(
                command.id.clone(),
                ChangeSetCheck {
                    command_id: command.id.clone(),
                    repository_id: command.repository.clone(),
                    status,
                    duration_ms: Some(duration_ms),
                    exit_code,
                    failure_code,
                },
            );
        }

        let mut change_set = inspect_task(&task)?;
        change_set.checks = checks;
        Ok(change_set)
    }
}
