use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::sync::Arc;

use super::provision::TaskWorkspaceProvisioner;
use super::provision_tests::ProjectFixture;
use super::runtime_ports::preferred_port;
use super::*;
use crate::project::model::{ProjectService, RuntimeIsolationSpec};

#[test]
fn broker_reserves_every_declared_port_and_exposes_the_task_environment() {
    let fixture = port_fixture();
    fixture.create_task("ports-one");
    let broker = TaskPortBroker::default();
    let provisioner = TaskWorkspaceProvisioner::with_port_broker(&fixture.states, &broker);
    provision_task(&fixture, &provisioner, "ports-one");

    let reserved = provisioner
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "ports-one",
        )
        .unwrap();

    assert_eq!(reserved.runtime.ports.len(), 2);
    assert_eq!(
        reserved
            .runtime
            .ports
            .values()
            .copied()
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    for (name, port) in &reserved.runtime.ports {
        assert!((49_152..=u16::MAX).contains(port));
        assert_eq!(
            broker
                .reserved_port(&reserved.runtime.namespace, name)
                .unwrap(),
            Some(*port)
        );
    }
    let environment = reserved.runtime.command_environment();
    assert_eq!(
        environment["GOWILD_PORT_API_SERVICE_HTTP"],
        reserved.runtime.ports["api-service.http"].to_string()
    );
    assert_eq!(
        environment["GOWILD_PORT_API_SERVICE_METRICS"],
        reserved.runtime.ports["api-service.metrics"].to_string()
    );

    let resumed = provisioner
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "ports-one",
        )
        .unwrap();
    assert_eq!(resumed, reserved);
}

#[test]
fn broker_records_a_busy_candidate_and_uses_the_next_bounded_port() {
    let fixture = port_fixture();
    fixture.create_task("busy-port");
    let broker = TaskPortBroker::default();
    let provisioner = TaskWorkspaceProvisioner::with_port_broker(&fixture.states, &broker);
    let provisioned = provision_task(&fixture, &provisioner, "busy-port");
    let name = "api-service.http";
    let preferred = preferred_port(&provisioned.runtime.namespace, name);
    let occupied = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, preferred)).unwrap();

    let reserved = provisioner
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "busy-port",
        )
        .unwrap();

    assert_ne!(reserved.runtime.ports[name], preferred);
    assert!(reserved.journal.iter().any(|transition| {
        transition.operation == TaskTransitionOperation::Acquire
            && transition.state == TaskTransitionState::Failed
            && transition.failure_code.as_deref() == Some("task_port_unavailable")
            && transition.resource
                == (OwnedResource::PortReservation {
                    name: name.into(),
                    port: preferred,
                })
    }));
    drop(occupied);
}

#[test]
fn broker_reconciles_a_port_bound_after_its_durable_plan() {
    let fixture = port_fixture();
    fixture.create_task("planned-port");
    let broker = TaskPortBroker::default();
    let provisioner = TaskWorkspaceProvisioner::with_port_broker(&fixture.states, &broker);
    let mut task = provision_task(&fixture, &provisioner, "planned-port");
    let name = "api-service.http";
    let port = preferred_port(&task.runtime.namespace, name);
    let expected_revision = task.revision;
    let sequence = task
        .plan_transition(
            TaskTransitionOperation::Acquire,
            OwnedResource::PortReservation {
                name: name.into(),
                port,
            },
        )
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();

    let recovered = provisioner
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "planned-port",
        )
        .unwrap();

    assert_eq!(recovered.runtime.ports[name], port);
    assert_eq!(
        recovered
            .journal
            .iter()
            .find(|transition| transition.sequence == sequence)
            .unwrap()
            .state,
        TaskTransitionState::Applied
    );
}

#[test]
fn broker_completes_a_port_release_interrupted_after_its_durable_plan() {
    let fixture = port_fixture();
    fixture.create_task("planned-port-release");
    let broker = TaskPortBroker::default();
    let provisioner = TaskWorkspaceProvisioner::with_port_broker(&fixture.states, &broker);
    provision_task(&fixture, &provisioner, "planned-port-release");
    let mut task = provisioner
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "planned-port-release",
        )
        .unwrap();
    let name = "api-service.http";
    let port = task.runtime.ports[name];
    let expected_revision = task.revision;
    let release_sequence = task
        .plan_transition(
            TaskTransitionOperation::Release,
            OwnedResource::PortReservation {
                name: name.into(),
                port,
            },
        )
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();

    let recovered = provisioner
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "planned-port-release",
        )
        .unwrap();

    assert_eq!(recovered.runtime.ports[name], port);
    assert_eq!(
        recovered
            .journal
            .iter()
            .find(|transition| transition.sequence == release_sequence)
            .unwrap()
            .state,
        TaskTransitionState::Applied
    );
    assert_eq!(
        broker
            .reserved_port(&recovered.runtime.namespace, name)
            .unwrap(),
        Some(port)
    );
}

#[test]
fn broker_retries_a_failed_port_release_after_external_completion() {
    let fixture = port_fixture();
    fixture.create_task("failed-port-release");
    let broker = TaskPortBroker::default();
    let provisioner = TaskWorkspaceProvisioner::with_port_broker(&fixture.states, &broker);
    provision_task(&fixture, &provisioner, "failed-port-release");
    let mut task = provisioner
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "failed-port-release",
        )
        .unwrap();
    let name = "api-service.http";
    let port = task.runtime.ports[name];
    let expected_revision = task.revision;
    let release_sequence = task
        .plan_transition(
            TaskTransitionOperation::Release,
            OwnedResource::PortReservation {
                name: name.into(),
                port,
            },
        )
        .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();
    provisioner.release_port(&task, name, port).unwrap();
    let expected_revision = task.revision;
    task.finish_transition(
        release_sequence,
        TaskTransitionState::Failed,
        Some("simulated_port_release_failure"),
    )
    .unwrap();
    fixture.states.save(&task, expected_revision).unwrap();

    let recovered = provisioner
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "failed-port-release",
        )
        .unwrap();

    assert_eq!(recovered.runtime.ports[name], port);
    assert!(recovered.journal.iter().any(|transition| {
        transition.operation == TaskTransitionOperation::Release
            && transition.state == TaskTransitionState::Applied
            && transition.resource
                == (OwnedResource::PortReservation {
                    name: name.into(),
                    port,
                })
    }));
}

#[test]
fn a_restarted_broker_rebinds_the_exact_persisted_ports() {
    let fixture = port_fixture();
    fixture.create_task("broker-restart");
    let first = {
        let broker = TaskPortBroker::default();
        let provisioner = TaskWorkspaceProvisioner::with_port_broker(&fixture.states, &broker);
        provision_task(&fixture, &provisioner, "broker-restart");
        provisioner
            .reserve_ports(
                &fixture.definition,
                &fixture.private_state,
                &fixture.project,
                "broker-restart",
            )
            .unwrap()
    };
    let restarted = TaskPortBroker::default();
    let provisioner = TaskWorkspaceProvisioner::with_port_broker(&fixture.states, &restarted);

    let recovered = provisioner
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "broker-restart",
        )
        .unwrap();

    assert_eq!(recovered, first);
    for (name, port) in &first.runtime.ports {
        assert_eq!(
            restarted
                .reserved_port(&first.runtime.namespace, name)
                .unwrap(),
            Some(*port)
        );
    }
}

#[test]
fn parallel_tasks_receive_distinct_process_owned_loopback_ports() {
    let fixture = port_fixture();
    fixture.create_task("parallel-ports-one");
    fixture.create_task("parallel-ports-two");
    let broker = Arc::new(TaskPortBroker::default());
    let states = Arc::new(fixture.states.clone());
    let definition = fixture.definition.clone();
    let private_state = fixture.private_state.clone();
    let project = fixture.project.clone();

    let handles = ["parallel-ports-one", "parallel-ports-two"].map(|task_id| {
        let broker = Arc::clone(&broker);
        let states = Arc::clone(&states);
        let definition = definition.clone();
        let private_state = private_state.clone();
        let project = project.clone();
        std::thread::spawn(move || {
            let provisioner = TaskWorkspaceProvisioner::with_port_broker(&states, &broker);
            provisioner.provision(&definition, &private_state, &project, task_id)?;
            provisioner.reserve_ports(&definition, &private_state, &project, task_id)
        })
    });
    let [first_handle, second_handle] = handles;
    let first = first_handle.join().unwrap().unwrap();
    let second = second_handle.join().unwrap().unwrap();

    let all_ports = first
        .runtime
        .ports
        .values()
        .chain(second.runtime.ports.values())
        .copied()
        .collect::<BTreeSet<_>>();
    assert_eq!(all_ports.len(), 4);
}

#[test]
fn cleanup_releases_durable_port_ownership_and_operating_system_leases() {
    let fixture = port_fixture();
    fixture.create_task("port-cleanup");
    let broker = TaskPortBroker::default();
    let provisioner = TaskWorkspaceProvisioner::with_port_broker(&fixture.states, &broker);
    provision_task(&fixture, &provisioner, "port-cleanup");
    let reserved = provisioner
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "port-cleanup",
        )
        .unwrap();
    let ports = reserved.runtime.ports.values().copied().collect::<Vec<_>>();

    let cleaned = provisioner.cleanup("port-cleanup").unwrap();

    assert_eq!(cleaned.phase, TaskWorkspacePhase::Cleaned);
    assert!(cleaned.runtime.ports.is_empty());
    assert!(!cleaned.root.exists());
    let rebound = ports
        .into_iter()
        .map(|port| TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rebound.len(), 2);
}

#[test]
fn cleanup_refuses_to_mutate_a_port_owning_task_without_its_broker() {
    let fixture = port_fixture();
    fixture.create_task("missing-broker");
    let broker = TaskPortBroker::default();
    let managed = TaskWorkspaceProvisioner::with_port_broker(&fixture.states, &broker);
    provision_task(&fixture, &managed, "missing-broker");
    let reserved = managed
        .reserve_ports(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "missing-broker",
        )
        .unwrap();

    let error = TaskWorkspaceProvisioner::new(&fixture.states)
        .cleanup("missing-broker")
        .unwrap_err();

    assert_eq!(error.code, "task_port_broker_required");
    let persisted = fixture.states.load("missing-broker").unwrap();
    assert_eq!(persisted, reserved);
    assert!(persisted.root.exists());
    for (name, port) in &persisted.runtime.ports {
        assert_eq!(
            broker
                .reserved_port(&persisted.runtime.namespace, name)
                .unwrap(),
            Some(*port)
        );
    }
}

fn port_fixture() -> ProjectFixture {
    let mut fixture = ProjectFixture::new(false);
    let service = ProjectService {
        id: "api-service".into(),
        repository: Some("api".into()),
        cwd: None,
        argv: vec!["run-api".into()],
        environment: Default::default(),
        isolation: RuntimeIsolationSpec {
            ports: vec!["http".into(), "metrics".into()],
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
