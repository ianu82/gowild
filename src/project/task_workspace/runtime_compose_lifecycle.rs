use super::provision::{
    require_matching_definition, verify_provisioned_task, TaskWorkspaceProvisioner,
};
use super::runtime_compose::{compose_control, prepare_compose_invocation, TaskComposeRuntime};
use super::{LoadedProject, OwnedResource, TaskWorkspace, TaskWorkspacePhase};
use crate::project::{ProjectDefinition, ProjectError, ProjectPrivateState};

impl TaskWorkspaceProvisioner<'_> {
    pub fn start_compose(
        &self,
        definition: &ProjectDefinition,
        private_state: &ProjectPrivateState,
        project: &LoadedProject,
        task_id: &str,
        runtime: &dyn TaskComposeRuntime,
    ) -> Result<TaskWorkspace, ProjectError> {
        require_matching_definition(definition, project)?;
        private_state.require_execution_trust(definition)?;
        let _operation_lock = self.states().lock_task_operations(task_id)?;
        let mut task = self.states().load(task_id)?;
        task.validate(project)?;
        if !matches!(
            task.phase,
            TaskWorkspacePhase::Ready
                | TaskWorkspacePhase::Running
                | TaskWorkspacePhase::Stopped
                | TaskWorkspacePhase::NeedsAttention
        ) {
            return Err(ProjectError::new(
                "task_workspace_not_startable",
                "task Compose can start only after workspace provisioning",
            ));
        }
        verify_provisioned_task(&task)?;
        self.verify_runtime_ports(&task)?;
        let Some(service) = project
            .manifest
            .services
            .iter()
            .find(|service| service.isolation.compose)
        else {
            return Ok(task);
        };
        let invocation = prepare_compose_invocation(&task, service)?;
        let resource = compose_resource(&task);
        if task.resource_is_owned(&resource) {
            match runtime.verify(&invocation.control) {
                Ok(()) => {}
                Err(error) if error.code == "task_compose_not_running" => {
                    let stop_control = invocation.control.clone();
                    self.ensure_released(
                        &mut task,
                        resource.clone(),
                        || Ok(()),
                        || runtime.down(&stop_control),
                    )?;
                }
                Err(error) => {
                    self.mark_compose_attention(&mut task)?;
                    return Err(error);
                }
            }
        }
        self.ensure_acquired(
            &mut task,
            resource,
            || runtime.verify(&invocation.control),
            || runtime.ensure_up(&invocation),
        )?;
        if task.phase != TaskWorkspacePhase::Running {
            self.transition_phase(&mut task, TaskWorkspacePhase::Running)?;
        }
        Ok(task)
    }

    pub fn stop_compose(
        &self,
        task_id: &str,
        runtime: &dyn TaskComposeRuntime,
    ) -> Result<TaskWorkspace, ProjectError> {
        let _operation_lock = self.states().lock_task_operations(task_id)?;
        let mut task = self.states().load(task_id)?;
        task.validate_integrity()?;
        if !matches!(
            task.phase,
            TaskWorkspacePhase::Ready
                | TaskWorkspacePhase::Running
                | TaskWorkspacePhase::Stopped
                | TaskWorkspacePhase::NeedsAttention
        ) {
            return Err(ProjectError::new(
                "task_workspace_not_stoppable",
                "task Compose can stop only from a provisioned workspace",
            ));
        }
        if task.runtime.compose_enabled {
            let resource = compose_resource(&task);
            if !task.resource_is_owned(&resource) {
                self.reconcile_unowned_compose_acquisition(&mut task, &resource, runtime)?;
            }
            if task.resource_is_owned(&resource) {
                let control = compose_control(&task);
                self.ensure_released(&mut task, resource, || Ok(()), || runtime.down(&control))?;
            }
        }
        if !task.owns_active_runtime_resources()
            && !task.has_unresolved_runtime_transition()
            && task.phase != TaskWorkspacePhase::Stopped
        {
            self.transition_phase(&mut task, TaskWorkspacePhase::Stopped)?;
        }
        Ok(task)
    }

    fn mark_compose_attention(&self, task: &mut TaskWorkspace) -> Result<(), ProjectError> {
        if task.phase != TaskWorkspacePhase::NeedsAttention {
            self.transition_phase(task, TaskWorkspacePhase::NeedsAttention)?;
        }
        Ok(())
    }

    fn reconcile_unowned_compose_acquisition(
        &self,
        task: &mut TaskWorkspace,
        resource: &OwnedResource,
        runtime: &dyn TaskComposeRuntime,
    ) -> Result<(), ProjectError> {
        let latest = task
            .journal
            .iter()
            .rev()
            .find(|transition| transition.resource == *resource);
        let Some(unresolved) = latest.filter(|transition| {
            transition.operation == super::TaskTransitionOperation::Acquire
                && matches!(
                    transition.state,
                    super::TaskTransitionState::Planned | super::TaskTransitionState::Failed
                )
        }) else {
            return Ok(());
        };
        let unresolved_sequence = unresolved.sequence;
        let unresolved_state = unresolved.state;
        let control = compose_control(task);
        if let Err(error) = runtime.down(&control) {
            self.mark_compose_attention(task)?;
            return Err(error);
        }
        match unresolved_state {
            super::TaskTransitionState::Planned => {
                let expected_revision = task.revision;
                task.finish_transition(
                    unresolved_sequence,
                    super::TaskTransitionState::RolledBack,
                    None,
                )?;
                self.states().save(task, expected_revision)
            }
            super::TaskTransitionState::Failed => {
                let expected_revision = task.revision;
                let sequence = task
                    .plan_transition(super::TaskTransitionOperation::Acquire, resource.clone())?;
                self.states().save(task, expected_revision)?;
                let expected_revision = task.revision;
                task.finish_transition(sequence, super::TaskTransitionState::RolledBack, None)?;
                self.states().save(task, expected_revision)
            }
            _ => Ok(()),
        }
    }
}

fn compose_resource(task: &TaskWorkspace) -> OwnedResource {
    OwnedResource::ComposeProject {
        name: task.runtime.compose_project.clone(),
    }
}
