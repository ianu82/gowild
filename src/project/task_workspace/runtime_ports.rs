use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::sync::{Mutex, MutexGuard};

use sha2::{Digest, Sha256};

use super::provision::{require_matching_definition, TaskWorkspaceProvisioner};
use super::runtime_layout::verify_runtime_layout;
use super::{
    LoadedProject, OwnedResource, TaskTransitionOperation, TaskTransitionState, TaskWorkspace,
    TaskWorkspacePhase,
};
use crate::project::{ProjectDefinition, ProjectError, ProjectPrivateState};

const PRIVATE_PORT_START: u16 = 49_152;
const PRIVATE_PORT_COUNT: u16 = 16_384;
const MAX_PORT_ATTEMPTS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PortLeaseKey {
    namespace: String,
    name: String,
}

/// Process-owned loopback listeners that prevent parallel GoWild tasks from
/// selecting the same declared port between allocation and service startup.
#[derive(Default)]
pub struct TaskPortBroker {
    leases: Mutex<BTreeMap<PortLeaseKey, TcpListener>>,
}

impl TaskPortBroker {
    pub fn reserved_port(&self, namespace: &str, name: &str) -> Result<Option<u16>, ProjectError> {
        let leases = self.lock()?;
        leases
            .get(&lease_key(namespace, name))
            .map(listener_port)
            .transpose()
    }

    fn reserve_exact(&self, namespace: &str, name: &str, port: u16) -> Result<(), ProjectError> {
        let key = lease_key(namespace, name);
        let mut leases = self.lock()?;
        if let Some(existing) = leases.get(&key) {
            return if listener_port(existing)? == port {
                Ok(())
            } else {
                Err(ProjectError::new(
                    "task_port_ownership_mismatch",
                    "port broker already holds a different lease for this task resource",
                ))
            };
        }
        let listener =
            TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AddrInUse {
                    ProjectError::new(
                        "task_port_unavailable",
                        format!("loopback port {port} is unavailable"),
                    )
                } else {
                    port_io_error("bind", &error)
                }
            })?;
        listener
            .set_nonblocking(true)
            .map_err(|error| port_io_error("nonblocking configuration", &error))?;
        leases.insert(key, listener);
        Ok(())
    }

    fn release_exact(&self, namespace: &str, name: &str, port: u16) -> Result<(), ProjectError> {
        let key = lease_key(namespace, name);
        let mut leases = self.lock()?;
        if let Some(existing) = leases.get(&key) {
            if listener_port(existing)? != port {
                return Err(ProjectError::new(
                    "task_port_ownership_mismatch",
                    "port broker lease does not match the task ownership journal",
                ));
            }
            leases.remove(&key);
        }
        Ok(())
    }

    pub(super) fn verify_exact(
        &self,
        namespace: &str,
        name: &str,
        port: u16,
    ) -> Result<(), ProjectError> {
        let leases = self.lock()?;
        if let Some(existing) = leases.get(&lease_key(namespace, name)) {
            if listener_port(existing)? != port {
                return Err(ProjectError::new(
                    "task_port_ownership_mismatch",
                    "port broker lease does not match the task ownership journal",
                ));
            }
        }
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, BTreeMap<PortLeaseKey, TcpListener>>, ProjectError> {
        self.leases.lock().map_err(|_| {
            ProjectError::new(
                "task_port_broker_unavailable",
                "task port broker state is unavailable",
            )
        })
    }
}

impl TaskWorkspaceProvisioner<'_> {
    pub fn reserve_ports(
        &self,
        definition: &ProjectDefinition,
        private_state: &ProjectPrivateState,
        project: &LoadedProject,
        task_id: &str,
    ) -> Result<TaskWorkspace, ProjectError> {
        require_matching_definition(definition, project)?;
        private_state.require_execution_trust(definition)?;
        let broker = self.port_broker()?;
        let _operation_lock = self.states().lock_task_operations(task_id)?;
        let mut task = self.states().load(task_id)?;
        task.validate(project)?;
        if !matches!(
            task.phase,
            TaskWorkspacePhase::Ready | TaskWorkspacePhase::Stopped
        ) {
            return Err(ProjectError::new(
                "task_workspace_not_port_reservable",
                "task ports can be reserved only after provisioning and while the task is stopped",
            ));
        }
        verify_runtime_layout(&task)?;

        for name in task.runtime.declared_ports.clone() {
            self.ensure_port_reserved(&mut task, broker, &name)?;
        }
        Ok(task)
    }

    pub(super) fn release_port(
        &self,
        task: &TaskWorkspace,
        name: &str,
        port: u16,
    ) -> Result<(), ProjectError> {
        self.port_broker()?
            .release_exact(&task.runtime.namespace, name, port)
    }

    pub(super) fn port_broker(&self) -> Result<&TaskPortBroker, ProjectError> {
        self.ports().ok_or_else(|| {
            ProjectError::new(
                "task_port_broker_required",
                "task owns loopback ports but this runtime has no port broker",
            )
        })
    }

    fn ensure_port_reserved(
        &self,
        task: &mut TaskWorkspace,
        broker: &TaskPortBroker,
        name: &str,
    ) -> Result<(), ProjectError> {
        if let Some(port) = task.runtime.ports.get(name).copied() {
            if self.recover_interrupted_port_release(task, broker, name, port)? {
                return self.ensure_port_reserved(task, broker, name);
            }
            match broker.reserve_exact(&task.runtime.namespace, name, port) {
                Ok(()) => return Ok(()),
                Err(error) if error.code == "task_port_unavailable" => {
                    self.release_stale_port(task, broker, name, port)?;
                }
                Err(error) => return Err(error),
            }
        }

        let planned_port = task.journal.iter().rev().find_map(|transition| {
            match (&transition.resource, transition.operation, transition.state) {
                (
                    OwnedResource::PortReservation {
                        name: planned_name,
                        port,
                    },
                    TaskTransitionOperation::Acquire,
                    TaskTransitionState::Planned,
                ) if planned_name == name => Some(*port),
                _ => None,
            }
        });
        let mut candidates = candidate_ports(&task.runtime.namespace, name);
        if let Some(port) = planned_port {
            candidates.retain(|candidate| *candidate != port);
            candidates.insert(0, port);
        }

        for port in candidates {
            let resource = OwnedResource::PortReservation {
                name: name.to_string(),
                port,
            };
            let sequence = if planned_port == Some(port) {
                task.journal
                    .iter()
                    .rev()
                    .find(|transition| {
                        transition.operation == TaskTransitionOperation::Acquire
                            && transition.state == TaskTransitionState::Planned
                            && transition.resource == resource
                    })
                    .map(|transition| transition.sequence)
                    .ok_or_else(|| {
                        ProjectError::new(
                            "invalid_task_workspace_journal",
                            "planned port reservation disappeared during recovery",
                        )
                    })?
            } else {
                let expected_revision = task.revision;
                let sequence = task.plan_transition(TaskTransitionOperation::Acquire, resource)?;
                self.states().save(task, expected_revision)?;
                sequence
            };

            match broker.reserve_exact(&task.runtime.namespace, name, port) {
                Ok(()) => {
                    let expected_revision = task.revision;
                    task.finish_transition(sequence, TaskTransitionState::Applied, None)?;
                    if let Err(error) = self.states().save(task, expected_revision) {
                        let _ = broker.release_exact(&task.runtime.namespace, name, port);
                        return Err(error);
                    }
                    return Ok(());
                }
                Err(error) if error.code == "task_port_unavailable" => {
                    self.finish_failed_attempt(task, sequence, error.code)?;
                }
                Err(error) => {
                    self.record_failed_transition(task, sequence, error.code)?;
                    return Err(error);
                }
            }
        }

        if task.phase != TaskWorkspacePhase::NeedsAttention {
            self.transition_phase(task, TaskWorkspacePhase::NeedsAttention)?;
        }
        Err(ProjectError::new(
            "task_port_allocation_exhausted",
            format!("no bounded loopback port is available for '{name}'"),
        ))
    }

    fn release_stale_port(
        &self,
        task: &mut TaskWorkspace,
        broker: &TaskPortBroker,
        name: &str,
        port: u16,
    ) -> Result<(), ProjectError> {
        let resource = OwnedResource::PortReservation {
            name: name.to_string(),
            port,
        };
        let expected_revision = task.revision;
        let sequence = task.plan_transition(TaskTransitionOperation::Release, resource)?;
        self.states().save(task, expected_revision)?;
        broker.release_exact(&task.runtime.namespace, name, port)?;
        let expected_revision = task.revision;
        task.finish_transition(sequence, TaskTransitionState::Applied, None)?;
        self.states().save(task, expected_revision)
    }

    fn recover_interrupted_port_release(
        &self,
        task: &mut TaskWorkspace,
        broker: &TaskPortBroker,
        name: &str,
        port: u16,
    ) -> Result<bool, ProjectError> {
        let resource = OwnedResource::PortReservation {
            name: name.to_string(),
            port,
        };
        let Some(state) = task.journal.iter().rev().find_map(|transition| {
            (transition.operation == TaskTransitionOperation::Release
                && transition.resource == resource)
                .then_some(transition.state)
        }) else {
            return Ok(false);
        };
        let sequence = match state {
            TaskTransitionState::Planned => task
                .journal
                .iter()
                .rev()
                .find(|transition| {
                    transition.operation == TaskTransitionOperation::Release
                        && transition.state == TaskTransitionState::Planned
                        && transition.resource == resource
                })
                .map(|transition| transition.sequence)
                .ok_or_else(|| {
                    ProjectError::new(
                        "invalid_task_workspace_journal",
                        "planned port release disappeared during recovery",
                    )
                })?,
            TaskTransitionState::Failed => {
                let expected_revision = task.revision;
                let sequence = task.plan_transition(TaskTransitionOperation::Release, resource)?;
                self.states().save(task, expected_revision)?;
                sequence
            }
            TaskTransitionState::Applied | TaskTransitionState::RolledBack => return Ok(false),
        };
        if let Err(error) = broker.release_exact(&task.runtime.namespace, name, port) {
            self.finish_failed_attempt(task, sequence, error.code)?;
            return Err(error);
        }
        let expected_revision = task.revision;
        task.finish_transition(sequence, TaskTransitionState::Applied, None)?;
        self.states().save(task, expected_revision)?;
        Ok(true)
    }

    fn finish_failed_attempt(
        &self,
        task: &mut TaskWorkspace,
        sequence: u64,
        failure_code: &'static str,
    ) -> Result<(), ProjectError> {
        let expected_revision = task.revision;
        task.finish_transition(sequence, TaskTransitionState::Failed, Some(failure_code))?;
        self.states().save(task, expected_revision)
    }
}

pub(super) fn preferred_port(namespace: &str, name: &str) -> u16 {
    candidate_ports(namespace, name)[0]
}

fn candidate_ports(namespace: &str, name: &str) -> Vec<u16> {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update([0]);
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let start = u16::from_be_bytes([digest[0], digest[1]]) % PRIVATE_PORT_COUNT;
    let step = (u16::from_be_bytes([digest[2], digest[3]]) | 1) % PRIVATE_PORT_COUNT;
    (0..MAX_PORT_ATTEMPTS)
        .map(|attempt| {
            let offset =
                start.wrapping_add((attempt as u16).wrapping_mul(step)) % PRIVATE_PORT_COUNT;
            PRIVATE_PORT_START + offset
        })
        .collect()
}

fn lease_key(namespace: &str, name: &str) -> PortLeaseKey {
    PortLeaseKey {
        namespace: namespace.to_string(),
        name: name.to_string(),
    }
}

fn listener_port(listener: &TcpListener) -> Result<u16, ProjectError> {
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| port_io_error("address inspection", &error))
}

fn port_io_error(operation: &'static str, error: &std::io::Error) -> ProjectError {
    ProjectError::new(
        "task_port_io",
        format!("task loopback port {operation} failed ({:?})", error.kind()),
    )
}
