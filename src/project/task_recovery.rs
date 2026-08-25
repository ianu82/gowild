use std::collections::BTreeSet;

use super::task_workspace::{
    OwnedResource, TaskTransition, TaskTransitionOperation, TaskTransitionState, TaskWorkspace,
    TaskWorkspacePhase,
};

/// The one safe next lifecycle action derived from durable task state.
///
/// These values deliberately do not claim that a previously running process is
/// still alive. Runtime liveness must be reconciled through the owning runtime
/// adapter after every server restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectTaskRecoveryAction {
    None,
    Provision,
    ResumeProvisioning,
    ResumeCleanup,
    ReconcileRuntime,
    ReviewAttention,
    ReviewProjectDefinition,
}

/// Secret-free recovery facts that remain valid across server restarts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTaskRecovery {
    pub action: ProjectTaskRecoveryAction,
    pub interrupted: bool,
    pub project_definition_changed: bool,
    pub runtime_verification_required: bool,
    pub pending_acquisitions: usize,
    pub pending_releases: usize,
    pub failed_acquisitions: usize,
    pub failed_releases: usize,
    pub owned_resource_count: usize,
    pub last_failure_code: Option<String>,
}

impl ProjectTaskRecovery {
    pub(crate) fn from_task(task: &TaskWorkspace, current_project: bool) -> Self {
        let unresolved = unresolved_transitions(task);
        let pending_acquisitions = count_transitions(
            &unresolved,
            TaskTransitionState::Planned,
            TaskTransitionOperation::Acquire,
        );
        let pending_releases = count_transitions(
            &unresolved,
            TaskTransitionState::Planned,
            TaskTransitionOperation::Release,
        );
        let failed_acquisitions = count_transitions(
            &unresolved,
            TaskTransitionState::Failed,
            TaskTransitionOperation::Acquire,
        );
        let failed_releases = count_transitions(
            &unresolved,
            TaskTransitionState::Failed,
            TaskTransitionOperation::Release,
        );
        let latest_unresolved = unresolved.first().copied();
        let owned_resources = owned_resources(task);
        let runtime_verification_required = matches!(task.phase, TaskWorkspacePhase::Running)
            || owned_resources.iter().any(is_runtime_resource)
            || unresolved
                .iter()
                .any(|transition| is_runtime_resource(&transition.resource));
        let action = recovery_action(
            task,
            current_project,
            latest_unresolved,
            runtime_verification_required,
        );

        Self {
            action,
            interrupted: matches!(
                task.phase,
                TaskWorkspacePhase::Provisioning | TaskWorkspacePhase::Cleaning
            ),
            project_definition_changed: !current_project,
            runtime_verification_required,
            pending_acquisitions,
            pending_releases,
            failed_acquisitions,
            failed_releases,
            owned_resource_count: owned_resources.len(),
            last_failure_code: unresolved
                .iter()
                .find_map(|transition| transition.failure_code.clone()),
        }
    }
}

fn recovery_action(
    task: &TaskWorkspace,
    current_project: bool,
    latest_unresolved: Option<&TaskTransition>,
    runtime_verification_required: bool,
) -> ProjectTaskRecoveryAction {
    use ProjectTaskRecoveryAction::{
        None, Provision, ReconcileRuntime, ResumeCleanup, ResumeProvisioning, ReviewAttention,
        ReviewProjectDefinition,
    };
    use TaskWorkspacePhase::{
        Cleaned, Cleaning, NeedsAttention, Planned, Provisioning, Ready, Running, Stopped,
    };

    match task.phase {
        Cleaning => ResumeCleanup,
        Cleaned => None,
        Running => ReconcileRuntime,
        Provisioning if current_project => ResumeProvisioning,
        Provisioning => ReviewProjectDefinition,
        Planned if current_project => Provision,
        Planned => ReviewProjectDefinition,
        Ready | Stopped if !current_project => ReviewProjectDefinition,
        Ready | Stopped => None,
        NeedsAttention if runtime_verification_required => ReconcileRuntime,
        NeedsAttention => match latest_unresolved {
            Some(transition) if transition.operation == TaskTransitionOperation::Release => {
                ResumeCleanup
            }
            Some(transition)
                if current_project && is_provisioning_resource(&transition.resource) =>
            {
                ResumeProvisioning
            }
            Some(_) if !current_project => ReviewProjectDefinition,
            _ => ReviewAttention,
        },
    }
}

/// Returns the newest unresolved record for each exact resource, newest first.
/// A later applied retry therefore resolves an older failure instead of leaving
/// a permanent false alarm.
fn unresolved_transitions(task: &TaskWorkspace) -> Vec<&TaskTransition> {
    let mut seen = BTreeSet::<&OwnedResource>::new();
    let mut unresolved = Vec::new();
    for transition in task.journal.iter().rev() {
        if !seen.insert(&transition.resource) {
            continue;
        }
        if matches!(
            transition.state,
            TaskTransitionState::Planned | TaskTransitionState::Failed
        ) {
            unresolved.push(transition);
        }
    }
    unresolved
}

fn count_transitions(
    transitions: &[&TaskTransition],
    state: TaskTransitionState,
    operation: TaskTransitionOperation,
) -> usize {
    transitions
        .iter()
        .filter(|transition| transition.state == state && transition.operation == operation)
        .count()
}

fn owned_resources(task: &TaskWorkspace) -> BTreeSet<OwnedResource> {
    let mut owned = BTreeSet::new();
    for transition in &task.journal {
        if transition.state != TaskTransitionState::Applied {
            continue;
        }
        match transition.operation {
            TaskTransitionOperation::Acquire => {
                owned.insert(transition.resource.clone());
            }
            TaskTransitionOperation::Release => {
                owned.remove(&transition.resource);
            }
        }
    }
    owned
}

fn is_provisioning_resource(resource: &OwnedResource) -> bool {
    matches!(
        resource,
        OwnedResource::WorkspaceDirectory { .. }
            | OwnedResource::RuntimeDirectory { .. }
            | OwnedResource::RepositoryWorktree { .. }
    )
}

fn is_runtime_resource(resource: &OwnedResource) -> bool {
    matches!(
        resource,
        OwnedResource::ComposeProject { .. } | OwnedResource::ServiceProcess { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::task_workspace::provision_tests::ProjectFixture;

    #[test]
    fn durable_phases_offer_one_explicit_restart_action() {
        let fixture = ProjectFixture::new(false);
        let task = fixture.create_task("recovery-phases");
        let cases = [
            (
                TaskWorkspacePhase::Planned,
                ProjectTaskRecoveryAction::Provision,
            ),
            (
                TaskWorkspacePhase::Provisioning,
                ProjectTaskRecoveryAction::ResumeProvisioning,
            ),
            (TaskWorkspacePhase::Ready, ProjectTaskRecoveryAction::None),
            (
                TaskWorkspacePhase::Running,
                ProjectTaskRecoveryAction::ReconcileRuntime,
            ),
            (TaskWorkspacePhase::Stopped, ProjectTaskRecoveryAction::None),
            (
                TaskWorkspacePhase::Cleaning,
                ProjectTaskRecoveryAction::ResumeCleanup,
            ),
            (TaskWorkspacePhase::Cleaned, ProjectTaskRecoveryAction::None),
        ];

        for (phase, expected) in cases {
            let mut candidate = task.clone();
            candidate.phase = phase;
            let recovery = ProjectTaskRecovery::from_task(&candidate, true);
            assert_eq!(recovery.action, expected);
            assert_eq!(
                recovery.interrupted,
                matches!(
                    phase,
                    TaskWorkspacePhase::Provisioning | TaskWorkspacePhase::Cleaning
                )
            );
        }
    }

    #[test]
    fn a_running_record_requires_runtime_reconciliation_not_liveness_inference() {
        let fixture = ProjectFixture::new(false);
        let mut task = fixture.create_task("runtime-recovery");
        task.phase = TaskWorkspacePhase::Running;

        let recovery = ProjectTaskRecovery::from_task(&task, true);

        assert_eq!(recovery.action, ProjectTaskRecoveryAction::ReconcileRuntime);
        assert!(recovery.runtime_verification_required);
    }

    #[test]
    fn unresolved_journal_work_reports_counts_failure_and_safe_retry() {
        let fixture = ProjectFixture::new(false);
        let mut task = fixture.create_task("journal-recovery");
        let resource = OwnedResource::WorkspaceDirectory {
            path: task.root.clone(),
        };
        let sequence = task
            .plan_transition(TaskTransitionOperation::Acquire, resource)
            .unwrap();
        task.finish_transition(
            sequence,
            TaskTransitionState::Failed,
            Some("task_workspace_root_io"),
        )
        .unwrap();
        task.phase = TaskWorkspacePhase::NeedsAttention;

        let recovery = ProjectTaskRecovery::from_task(&task, true);

        assert_eq!(
            recovery.action,
            ProjectTaskRecoveryAction::ResumeProvisioning
        );
        assert_eq!(recovery.failed_acquisitions, 1);
        assert_eq!(
            recovery.last_failure_code.as_deref(),
            Some("task_workspace_root_io")
        );
    }

    #[test]
    fn a_later_applied_retry_clears_the_older_failure_fact() {
        let fixture = ProjectFixture::new(false);
        let mut task = fixture.create_task("resolved-recovery");
        let resource = OwnedResource::WorkspaceDirectory {
            path: task.root.clone(),
        };
        let failed = task
            .plan_transition(TaskTransitionOperation::Acquire, resource.clone())
            .unwrap();
        task.finish_transition(failed, TaskTransitionState::Failed, Some("temporary_io"))
            .unwrap();
        let retried = task
            .plan_transition(TaskTransitionOperation::Acquire, resource)
            .unwrap();
        task.finish_transition(retried, TaskTransitionState::Applied, None)
            .unwrap();
        task.phase = TaskWorkspacePhase::Ready;

        let recovery = ProjectTaskRecovery::from_task(&task, true);

        assert_eq!(recovery.failed_acquisitions, 0);
        assert_eq!(recovery.last_failure_code, None);
        assert_eq!(recovery.owned_resource_count, 1);
    }

    #[test]
    fn a_failed_release_remains_owned_and_resumes_cleanup() {
        let fixture = ProjectFixture::new(false);
        let mut task = fixture.create_task("release-recovery");
        let resource = OwnedResource::WorkspaceDirectory {
            path: task.root.clone(),
        };
        let acquired = task
            .plan_transition(TaskTransitionOperation::Acquire, resource.clone())
            .unwrap();
        task.finish_transition(acquired, TaskTransitionState::Applied, None)
            .unwrap();
        let release = task
            .plan_transition(TaskTransitionOperation::Release, resource)
            .unwrap();
        task.finish_transition(release, TaskTransitionState::Failed, Some("cleanup_busy"))
            .unwrap();
        task.phase = TaskWorkspacePhase::NeedsAttention;

        let recovery = ProjectTaskRecovery::from_task(&task, true);

        assert_eq!(recovery.action, ProjectTaskRecoveryAction::ResumeCleanup);
        assert_eq!(recovery.failed_releases, 1);
        assert_eq!(recovery.owned_resource_count, 1);
    }

    #[test]
    fn stale_execution_is_never_offered_as_an_automatic_retry() {
        let fixture = ProjectFixture::new(false);
        let mut task = fixture.create_task("stale-recovery");
        task.phase = TaskWorkspacePhase::Provisioning;

        let recovery = ProjectTaskRecovery::from_task(&task, false);

        assert_eq!(
            recovery.action,
            ProjectTaskRecoveryAction::ReviewProjectDefinition
        );
        assert!(recovery.project_definition_changed);
    }
}
