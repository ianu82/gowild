use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use super::provision::TaskWorkspaceProvisioner;
use super::provision_tests::{persist_phase, ProjectFixture};
use super::rules::service_instance_id;
use super::runtime_services::{TaskServiceControl, TaskServiceInvocation, TaskServiceRuntime};
use super::*;
use crate::project::model::{ProjectService, RuntimeIsolationSpec};

#[derive(Default)]
struct FakeTaskServiceRuntime {
    services: Mutex<BTreeMap<String, (TaskServiceControl, bool)>>,
    invocations: Mutex<Vec<TaskServiceInvocation>>,
    launches: AtomicUsize,
    fail_next_release: AtomicBool,
    fail_next_stop: AtomicBool,
}

impl FakeTaskServiceRuntime {
    fn launch_count(&self) -> usize {
        self.launches.load(Ordering::SeqCst)
    }

    fn invocations(&self) -> Vec<TaskServiceInvocation> {
        self.invocations.lock().unwrap().clone()
    }

    fn seed_waiting(&self, control: TaskServiceControl) {
        self.services
            .lock()
            .unwrap()
            .insert(control.instance_id.clone(), (control, false));
    }

    fn fail_next_release(&self) {
        self.fail_next_release.store(true, Ordering::SeqCst);
    }

    fn fail_next_stop(&self) {
        self.fail_next_stop.store(true, Ordering::SeqCst);
    }

    fn drop_service(&self, instance_id: &str) {
        self.services.lock().unwrap().remove(instance_id);
    }
}

impl TaskServiceRuntime for FakeTaskServiceRuntime {
    fn ensure_waiting(&self, invocation: &TaskServiceInvocation) -> Result<(), ProjectError> {
        let mut services = self.services.lock().unwrap();
        if let Some((control, _)) = services.get(&invocation.control.instance_id) {
            if control == &invocation.control {
                return Ok(());
            }
            return Err(ProjectError::new(
                "fake_service_identity_mismatch",
                "fake service instance has conflicting control data",
            ));
        }
        services.insert(
            invocation.control.instance_id.clone(),
            (invocation.control.clone(), false),
        );
        self.invocations.lock().unwrap().push(invocation.clone());
        self.launches.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn verify(&self, control: &TaskServiceControl) -> Result<(), ProjectError> {
        match self.services.lock().unwrap().get(&control.instance_id) {
            Some((existing, _)) if existing == control => Ok(()),
            _ => Err(ProjectError::new(
                "task_service_not_running",
                "fake service instance is missing",
            )),
        }
    }

    fn release_start(&self, control: &TaskServiceControl) -> Result<(), ProjectError> {
        if self.fail_next_release.swap(false, Ordering::SeqCst) {
            return Err(ProjectError::new(
                "fake_service_start_failed",
                "fake service start failed",
            ));
        }
        let mut services = self.services.lock().unwrap();
        let Some((existing, started)) = services.get_mut(&control.instance_id) else {
            return Err(ProjectError::new(
                "fake_service_missing",
                "fake service instance is missing",
            ));
        };
        if existing != control {
            return Err(ProjectError::new(
                "fake_service_identity_mismatch",
                "fake service instance has conflicting control data",
            ));
        }
        *started = true;
        Ok(())
    }

    fn stop(&self, control: &TaskServiceControl) -> Result<(), ProjectError> {
        if self.fail_next_stop.swap(false, Ordering::SeqCst) {
            return Err(ProjectError::new(
                "fake_service_stop_failed",
                "fake service stop failed",
            ));
        }
        let mut services = self.services.lock().unwrap();
        if let Some((existing, _)) = services.get(&control.instance_id) {
            if existing != control {
                return Err(ProjectError::new(
                    "fake_service_identity_mismatch",
                    "fake service instance has conflicting control data",
                ));
            }
        }
        services.remove(&control.instance_id);
        Ok(())
    }
}

#[test]
fn parallel_tasks_run_the_same_service_in_distinct_workspaces() {
    let fixture = service_fixture(false);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let runtime = FakeTaskServiceRuntime::default();
    for task_id in ["service-one", "service-two"] {
        fixture.create_task(task_id);
        provision_task(&fixture, &provisioner, task_id);
    }

    let first = start_task(&fixture, &provisioner, "service-one", &runtime);
    let second = start_task(&fixture, &provisioner, "service-two", &runtime);

    assert_eq!(first.phase, TaskWorkspacePhase::Running);
    assert_eq!(second.phase, TaskWorkspacePhase::Running);
    let invocations = runtime.invocations();
    assert_eq!(invocations.len(), 2);
    assert_ne!(
        invocations[0].control.instance_id,
        invocations[1].control.instance_id
    );
    assert_ne!(invocations[0].cwd, invocations[1].cwd);
    for (invocation, task) in invocations.iter().zip([&first, &second]) {
        assert_eq!(invocation.argv, ["run-api"]);
        assert_eq!(invocation.environment["FEATURE_MODE"], "isolated");
        assert_eq!(
            invocation.environment["COMPOSE_PROJECT_NAME"],
            task.runtime.namespace
        );
        assert_eq!(
            invocation.cwd,
            task.repositories["api"]
                .worktree
                .as_ref()
                .unwrap()
                .checkout_path
                .canonicalize()
                .unwrap()
        );
        assert!(task.resource_is_owned(&OwnedResource::ServiceProcess {
            service_id: "api-service".into(),
            instance_id: invocation.control.instance_id.clone(),
        }));
    }

    let resumed = start_task(&fixture, &provisioner, "service-one", &runtime);
    assert_eq!(resumed.phase, TaskWorkspacePhase::Running);
    assert_eq!(runtime.launch_count(), 2);

    let first_instance = service_instance_id(&first.runtime.namespace, "api-service");
    runtime.drop_service(&first_instance);
    let restarted = start_task(&fixture, &provisioner, "service-one", &runtime);
    assert_eq!(restarted.phase, TaskWorkspacePhase::Running);
    assert_eq!(runtime.launch_count(), 3);
    assert!(restarted.journal.iter().any(|transition| {
        transition.operation == TaskTransitionOperation::Release
            && transition.state == TaskTransitionState::Applied
            && transition.resource
                == (OwnedResource::ServiceProcess {
                    service_id: "api-service".into(),
                    instance_id: first_instance.clone(),
                })
    }));

    assert_eq!(
        provisioner
            .stop_services("service-one", &runtime)
            .unwrap()
            .phase,
        TaskWorkspacePhase::Stopped
    );
    assert_eq!(
        provisioner
            .stop_services("service-two", &runtime)
            .unwrap()
            .phase,
        TaskWorkspacePhase::Stopped
    );
}

#[test]
fn planned_service_acquisition_recovers_without_a_duplicate_launch() {
    let fixture = service_fixture(false);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    fixture.create_task("planned-service");
    let mut task = provision_task(&fixture, &provisioner, "planned-service");
    let control = control_for(&task, "api-service");
    let resource = service_resource(&control);
    let expected_revision = task.revision;
    let sequence = task
        .plan_transition(TaskTransitionOperation::Acquire, resource.clone())
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    let runtime = FakeTaskServiceRuntime::default();
    runtime.seed_waiting(control);

    let recovered = start_task(&fixture, &provisioner, "planned-service", &runtime);

    assert_eq!(recovered.phase, TaskWorkspacePhase::Running);
    assert_eq!(runtime.launch_count(), 0);
    assert!(recovered.journal.iter().any(|transition| {
        transition.sequence == sequence
            && transition.state == TaskTransitionState::Applied
            && transition.resource == resource
    }));
}

#[test]
fn failed_start_signal_keeps_ownership_and_resumes_safely() {
    let fixture = service_fixture(false);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    fixture.create_task("release-recovery");
    provision_task(&fixture, &provisioner, "release-recovery");
    let runtime = FakeTaskServiceRuntime::default();
    runtime.fail_next_release();

    let error = provisioner
        .start_services(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "release-recovery",
            &runtime,
        )
        .unwrap_err();

    assert_eq!(error.code, "fake_service_start_failed");
    let interrupted = fixture.states.load("release-recovery").unwrap();
    assert_eq!(interrupted.phase, TaskWorkspacePhase::NeedsAttention);
    assert!(
        interrupted.resource_is_owned(&service_resource(&control_for(&interrupted, "api-service")))
    );

    let recovered = start_task(&fixture, &provisioner, "release-recovery", &runtime);
    assert_eq!(recovered.phase, TaskWorkspacePhase::Running);
    assert_eq!(runtime.launch_count(), 1);
}

#[test]
fn failed_stop_is_recoverable_and_cleanup_refuses_owned_services() {
    let fixture = service_fixture(false);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    fixture.create_task("stop-recovery");
    provision_task(&fixture, &provisioner, "stop-recovery");
    let runtime = FakeTaskServiceRuntime::default();
    let mut running = start_task(&fixture, &provisioner, "stop-recovery", &runtime);

    assert_eq!(
        provisioner.cleanup("stop-recovery").unwrap_err().code,
        "task_workspace_running"
    );
    persist_phase(&fixture.states, &mut running, TaskWorkspacePhase::Stopped);
    assert_eq!(
        provisioner.cleanup("stop-recovery").unwrap_err().code,
        "task_runtime_still_owned"
    );

    runtime.fail_next_stop();
    assert_eq!(
        provisioner
            .stop_services("stop-recovery", &runtime)
            .unwrap_err()
            .code,
        "fake_service_stop_failed"
    );
    assert_eq!(
        fixture.states.load("stop-recovery").unwrap().phase,
        TaskWorkspacePhase::NeedsAttention
    );

    let stopped = provisioner
        .stop_services("stop-recovery", &runtime)
        .unwrap();
    assert_eq!(stopped.phase, TaskWorkspacePhase::Stopped);
    assert!(!stopped.resource_is_owned(&service_resource(&control_for(&stopped, "api-service"))));
    assert_eq!(
        provisioner.cleanup("stop-recovery").unwrap().phase,
        TaskWorkspacePhase::Cleaned
    );
}

#[test]
fn service_start_rechecks_trust_and_routes_compose_elsewhere() {
    let fixture = service_fixture(false);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    fixture.create_task("untrusted-service");
    provision_task(&fixture, &provisioner, "untrusted-service");
    let runtime = FakeTaskServiceRuntime::default();
    let mut untrusted = fixture.private_state.clone();
    assert!(untrusted.revoke_trust());

    assert_eq!(
        provisioner
            .start_services(
                &fixture.definition,
                &untrusted,
                &fixture.project,
                "untrusted-service",
                &runtime,
            )
            .unwrap_err()
            .code,
        "project_manifest_untrusted"
    );
    assert_eq!(runtime.launch_count(), 0);

    let compose = service_fixture(true);
    let compose_provisioner = TaskWorkspaceProvisioner::new(&compose.states);
    compose.create_task("compose-service");
    provision_task(&compose, &compose_provisioner, "compose-service");
    assert_eq!(
        compose_provisioner
            .start_services(
                &compose.definition,
                &compose.private_state,
                &compose.project,
                "compose-service",
                &runtime,
            )
            .unwrap_err()
            .code,
        "task_compose_runtime_required"
    );
    assert_eq!(runtime.launch_count(), 0);
}

fn service_fixture(compose: bool) -> ProjectFixture {
    let mut fixture = ProjectFixture::new(false);
    let service = ProjectService {
        id: "api-service".into(),
        repository: Some("api".into()),
        cwd: None,
        argv: if compose {
            vec![
                "docker".into(),
                "compose".into(),
                "up".into(),
                "--detach".into(),
            ]
        } else {
            vec!["run-api".into()]
        },
        environment: BTreeMap::from([("FEATURE_MODE".into(), "isolated".into())]),
        isolation: RuntimeIsolationSpec {
            compose,
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
    runtime: &dyn TaskServiceRuntime,
) -> TaskWorkspace {
    provisioner
        .start_services(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            task_id,
            runtime,
        )
        .unwrap()
}

fn control_for(task: &TaskWorkspace, service_id: &str) -> TaskServiceControl {
    let directory = task
        .runtime
        .root
        .join("control")
        .join("services")
        .join(service_id);
    TaskServiceControl {
        service_id: service_id.into(),
        instance_id: service_instance_id(&task.runtime.namespace, service_id),
        lease_path: directory.join("lease.json"),
        start_path: directory.join("start"),
        directory,
    }
}

fn service_resource(control: &TaskServiceControl) -> OwnedResource {
    OwnedResource::ServiceProcess {
        service_id: control.service_id.clone(),
        instance_id: control.instance_id.clone(),
    }
}
