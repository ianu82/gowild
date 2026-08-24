use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use super::provision::TaskWorkspaceProvisioner;
use super::provision_tests::ProjectFixture;
use super::rules::service_instance_id;
use super::runtime_compose::{
    prepare_compose_invocation, TaskComposeControl, TaskComposeInvocation, TaskComposeRuntime,
};
use super::runtime_services::{TaskServiceControl, TaskServiceInvocation, TaskServiceRuntime};
use super::*;
use crate::project::model::{ProjectService, RuntimeIsolationSpec};

#[derive(Default)]
struct FakeTaskComposeRuntime {
    stacks: Mutex<BTreeMap<String, TaskComposeControl>>,
    invocations: Mutex<Vec<TaskComposeInvocation>>,
    starts: AtomicUsize,
    fail_next_start: AtomicBool,
    fail_next_stop: AtomicBool,
}

impl FakeTaskComposeRuntime {
    fn start_count(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }

    fn invocations(&self) -> Vec<TaskComposeInvocation> {
        self.invocations.lock().unwrap().clone()
    }

    fn seed_running(&self, control: TaskComposeControl) {
        self.stacks
            .lock()
            .unwrap()
            .insert(control.project_name.clone(), control);
    }

    fn drop_stack(&self, project_name: &str) {
        self.stacks.lock().unwrap().remove(project_name);
    }

    fn fail_next_start(&self) {
        self.fail_next_start.store(true, Ordering::SeqCst);
    }

    fn fail_next_stop(&self) {
        self.fail_next_stop.store(true, Ordering::SeqCst);
    }
}

impl TaskComposeRuntime for FakeTaskComposeRuntime {
    fn ensure_up(&self, invocation: &TaskComposeInvocation) -> Result<(), ProjectError> {
        if self.fail_next_start.swap(false, Ordering::SeqCst) {
            return Err(ProjectError::new(
                "fake_compose_start_failed",
                "fake Compose start failed",
            ));
        }
        let mut stacks = self.stacks.lock().unwrap();
        if let Some(control) = stacks.get(&invocation.control.project_name) {
            return if control == &invocation.control {
                Ok(())
            } else {
                Err(ProjectError::new(
                    "fake_compose_identity_mismatch",
                    "fake Compose stack has conflicting control data",
                ))
            };
        }
        stacks.insert(
            invocation.control.project_name.clone(),
            invocation.control.clone(),
        );
        self.invocations.lock().unwrap().push(invocation.clone());
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn verify(&self, control: &TaskComposeControl) -> Result<(), ProjectError> {
        match self.stacks.lock().unwrap().get(&control.project_name) {
            Some(existing) if existing == control => Ok(()),
            _ => Err(ProjectError::new(
                "task_compose_not_running",
                "fake Compose stack is not running",
            )),
        }
    }

    fn down(&self, control: &TaskComposeControl) -> Result<(), ProjectError> {
        if self.fail_next_stop.swap(false, Ordering::SeqCst) {
            return Err(ProjectError::new(
                "fake_compose_stop_failed",
                "fake Compose stop failed",
            ));
        }
        let mut stacks = self.stacks.lock().unwrap();
        if let Some(existing) = stacks.get(&control.project_name) {
            if existing != control {
                return Err(ProjectError::new(
                    "fake_compose_identity_mismatch",
                    "fake Compose stack has conflicting control data",
                ));
            }
        }
        stacks.remove(&control.project_name);
        Ok(())
    }
}

struct UnusedTaskServiceRuntime;

impl TaskServiceRuntime for UnusedTaskServiceRuntime {
    fn ensure_waiting(&self, _invocation: &TaskServiceInvocation) -> Result<(), ProjectError> {
        Err(unexpected_process_runtime())
    }

    fn verify(&self, _control: &TaskServiceControl) -> Result<(), ProjectError> {
        Err(unexpected_process_runtime())
    }

    fn release_start(&self, _control: &TaskServiceControl) -> Result<(), ProjectError> {
        Err(unexpected_process_runtime())
    }

    fn stop(&self, _control: &TaskServiceControl) -> Result<(), ProjectError> {
        Err(unexpected_process_runtime())
    }
}

#[test]
fn parallel_tasks_run_distinct_compose_projects_and_resume_without_duplicates() {
    let fixture = compose_fixture(false);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let runtime = FakeTaskComposeRuntime::default();
    for task_id in ["compose-one", "compose-two"] {
        fixture.create_task(task_id);
        provision_task(&fixture, &provisioner, task_id);
    }

    let first = start_task(&fixture, &provisioner, "compose-one", &runtime);
    let second = start_task(&fixture, &provisioner, "compose-two", &runtime);
    let invocations = runtime.invocations();

    assert_eq!(first.phase, TaskWorkspacePhase::Running);
    assert_eq!(second.phase, TaskWorkspacePhase::Running);
    assert_eq!(invocations.len(), 2);
    assert_ne!(
        invocations[0].control.project_name,
        invocations[1].control.project_name
    );
    assert_ne!(invocations[0].cwd, invocations[1].cwd);
    assert_eq!(invocations[0].environment["FEATURE_MODE"], "isolated");
    assert_eq!(runtime.start_count(), 2);

    start_task(&fixture, &provisioner, "compose-one", &runtime);
    assert_eq!(runtime.start_count(), 2);
    runtime.drop_stack(&first.runtime.compose_project);
    let restarted = start_task(&fixture, &provisioner, "compose-one", &runtime);
    assert_eq!(runtime.start_count(), 3);
    assert!(restarted.journal.iter().any(|transition| {
        transition.operation == TaskTransitionOperation::Release
            && transition.state == TaskTransitionState::Applied
            && transition.resource == compose_resource(&restarted)
    }));

    assert_eq!(
        provisioner
            .stop_compose("compose-one", &runtime)
            .unwrap()
            .phase,
        TaskWorkspacePhase::Stopped
    );
    assert_eq!(
        provisioner
            .stop_compose("compose-two", &runtime)
            .unwrap()
            .phase,
        TaskWorkspacePhase::Stopped
    );
}

#[test]
fn planned_compose_acquisition_recovers_without_duplicate_start() {
    let fixture = compose_fixture(false);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    fixture.create_task("compose-planned");
    let mut task = provision_task(&fixture, &provisioner, "compose-planned");
    let invocation =
        prepare_compose_invocation(&task, &fixture.project.manifest.services[0]).unwrap();
    let resource = compose_resource(&task);
    let expected_revision = task.revision;
    let sequence = task
        .plan_transition(TaskTransitionOperation::Acquire, resource.clone())
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    let runtime = FakeTaskComposeRuntime::default();
    runtime.seed_running(invocation.control);

    let recovered = start_task(&fixture, &provisioner, "compose-planned", &runtime);

    assert_eq!(recovered.phase, TaskWorkspacePhase::Running);
    assert_eq!(runtime.start_count(), 0);
    assert!(recovered.journal.iter().any(|transition| {
        transition.sequence == sequence
            && transition.state == TaskTransitionState::Applied
            && transition.resource == resource
    }));
}

#[test]
fn planned_compose_acquisition_can_be_stopped_and_rolled_back() {
    let fixture = compose_fixture(false);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    fixture.create_task("compose-planned-stop");
    let mut task = provision_task(&fixture, &provisioner, "compose-planned-stop");
    let invocation =
        prepare_compose_invocation(&task, &fixture.project.manifest.services[0]).unwrap();
    let resource = compose_resource(&task);
    let expected_revision = task.revision;
    let sequence = task
        .plan_transition(TaskTransitionOperation::Acquire, resource)
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    let runtime = FakeTaskComposeRuntime::default();
    runtime.seed_running(invocation.control);

    let stopped = provisioner
        .stop_compose("compose-planned-stop", &runtime)
        .unwrap();

    assert_eq!(stopped.phase, TaskWorkspacePhase::Stopped);
    assert_eq!(
        stopped
            .journal
            .iter()
            .find(|transition| transition.sequence == sequence)
            .unwrap()
            .state,
        TaskTransitionState::RolledBack
    );
    assert!(!stopped.has_unresolved_runtime_transition());
    assert_eq!(
        provisioner.cleanup("compose-planned-stop").unwrap().phase,
        TaskWorkspacePhase::Cleaned
    );
}

#[test]
fn failed_compose_start_and_stop_remain_recoverable_and_gate_cleanup() {
    let fixture = compose_fixture(false);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    fixture.create_task("compose-failure");
    provision_task(&fixture, &provisioner, "compose-failure");
    let runtime = FakeTaskComposeRuntime::default();
    runtime.fail_next_start();

    assert_eq!(
        provisioner
            .start_compose(
                &fixture.definition,
                &fixture.private_state,
                &fixture.project,
                "compose-failure",
                &runtime,
            )
            .unwrap_err()
            .code,
        "fake_compose_start_failed"
    );
    assert_eq!(
        fixture.states.load("compose-failure").unwrap().phase,
        TaskWorkspacePhase::NeedsAttention
    );
    assert_eq!(
        provisioner.cleanup("compose-failure").unwrap_err().code,
        "task_runtime_still_owned"
    );
    assert_eq!(
        provisioner
            .stop_services("compose-failure", &UnusedTaskServiceRuntime)
            .unwrap()
            .phase,
        TaskWorkspacePhase::NeedsAttention
    );
    assert_eq!(
        provisioner
            .stop_compose("compose-failure", &runtime)
            .unwrap()
            .phase,
        TaskWorkspacePhase::Stopped
    );

    let running = start_task(&fixture, &provisioner, "compose-failure", &runtime);
    assert!(running.resource_is_owned(&compose_resource(&running)));
    assert_eq!(
        provisioner.cleanup("compose-failure").unwrap_err().code,
        "task_workspace_running"
    );
    runtime.fail_next_stop();
    assert_eq!(
        provisioner
            .stop_compose("compose-failure", &runtime)
            .unwrap_err()
            .code,
        "fake_compose_stop_failed"
    );
    assert_eq!(
        provisioner.cleanup("compose-failure").unwrap_err().code,
        "task_runtime_still_owned"
    );

    let stopped = provisioner
        .stop_compose("compose-failure", &runtime)
        .unwrap();
    assert_eq!(stopped.phase, TaskWorkspacePhase::Stopped);
    assert!(!stopped.resource_is_owned(&compose_resource(&stopped)));
    assert_eq!(
        provisioner.cleanup("compose-failure").unwrap().phase,
        TaskWorkspacePhase::Cleaned
    );
}

#[test]
fn compose_start_rechecks_trust_and_reserved_ports() {
    let fixture = compose_fixture(true);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    fixture.create_task("compose-gates");
    provision_task(&fixture, &provisioner, "compose-gates");
    let runtime = FakeTaskComposeRuntime::default();
    let mut untrusted = fixture.private_state.clone();
    assert!(untrusted.revoke_trust());

    assert_eq!(
        provisioner
            .start_compose(
                &fixture.definition,
                &untrusted,
                &fixture.project,
                "compose-gates",
                &runtime,
            )
            .unwrap_err()
            .code,
        "project_manifest_untrusted"
    );
    assert_eq!(
        provisioner
            .start_compose(
                &fixture.definition,
                &fixture.private_state,
                &fixture.project,
                "compose-gates",
                &runtime,
            )
            .unwrap_err()
            .code,
        "task_ports_not_ready"
    );
    assert_eq!(runtime.start_count(), 0);
}

#[test]
fn process_and_compose_stop_paths_preserve_the_other_runtime_owner() {
    let mut fixture = compose_fixture(false);
    let process = ProjectService {
        id: "worker".into(),
        repository: Some("api".into()),
        cwd: None,
        argv: vec!["run-worker".into()],
        environment: BTreeMap::new(),
        isolation: RuntimeIsolationSpec::default(),
    };
    fixture.definition.manifest.services.push(process.clone());
    fixture.project.manifest.services.push(process);
    fixture.definition.manifest.validate().unwrap();
    fixture.project.manifest.validate().unwrap();
    let digest = fixture.definition.digest.clone();
    fixture
        .private_state
        .grant_trust(&fixture.definition, &digest)
        .unwrap();
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    fixture.create_task("compose-mixed");
    provision_task(&fixture, &provisioner, "compose-mixed");
    let runtime = FakeTaskComposeRuntime::default();
    let mut task = start_task(&fixture, &provisioner, "compose-mixed", &runtime);

    assert_eq!(
        provisioner
            .stop_services("compose-mixed", &UnusedTaskServiceRuntime)
            .unwrap()
            .phase,
        TaskWorkspacePhase::Running
    );

    let process_resource = OwnedResource::ServiceProcess {
        service_id: "worker".into(),
        instance_id: service_instance_id(&task.runtime.namespace, "worker"),
    };
    let expected_revision = task.revision;
    let sequence = task
        .plan_transition(TaskTransitionOperation::Acquire, process_resource.clone())
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    let expected_revision = task.revision;
    task.finish_transition(sequence, TaskTransitionState::Applied, None)
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();

    let compose_stopped = provisioner.stop_compose("compose-mixed", &runtime).unwrap();
    assert_eq!(compose_stopped.phase, TaskWorkspacePhase::Running);
    assert!(compose_stopped.resource_is_owned(&process_resource));
    assert!(!compose_stopped.resource_is_owned(&compose_resource(&compose_stopped)));
}

fn compose_fixture(with_port: bool) -> ProjectFixture {
    let mut fixture = ProjectFixture::new(false);
    let service = ProjectService {
        id: "app-stack".into(),
        repository: Some("api".into()),
        cwd: None,
        argv: vec![
            "docker".into(),
            "compose".into(),
            "--file".into(),
            "README.md".into(),
            "up".into(),
            "--detach".into(),
        ],
        environment: BTreeMap::from([("FEATURE_MODE".into(), "isolated".into())]),
        isolation: RuntimeIsolationSpec {
            ports: with_port.then(|| "http".into()).into_iter().collect(),
            compose: true,
            ..RuntimeIsolationSpec::default()
        },
    };
    fixture.definition.manifest.services = vec![service.clone()];
    fixture.project.manifest.services = vec![service];
    fixture.definition.manifest.validate().unwrap();
    fixture.project.manifest.validate().unwrap();
    let digest = fixture.definition.digest.clone();
    fixture
        .private_state
        .grant_trust(&fixture.definition, &digest)
        .unwrap();
    fixture
}

fn provision_task(
    fixture: &ProjectFixture,
    provisioner: &TaskWorkspaceProvisioner<'_>,
    task_id: &str,
) -> TaskWorkspace {
    provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            task_id,
        )
        .unwrap()
}

fn start_task(
    fixture: &ProjectFixture,
    provisioner: &TaskWorkspaceProvisioner<'_>,
    task_id: &str,
    runtime: &dyn TaskComposeRuntime,
) -> TaskWorkspace {
    provisioner
        .start_compose(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            task_id,
            runtime,
        )
        .unwrap()
}

fn compose_resource(task: &TaskWorkspace) -> OwnedResource {
    OwnedResource::ComposeProject {
        name: task.runtime.compose_project.clone(),
    }
}

fn unexpected_process_runtime() -> ProjectError {
    ProjectError::new(
        "unexpected_process_runtime",
        "process runtime must not receive Compose services",
    )
}
