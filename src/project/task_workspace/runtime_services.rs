use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::provision::{
    require_matching_definition, verify_provisioned_task, TaskWorkspaceProvisioner,
};
use super::rules::service_instance_id;
use super::runtime_commands::resolve_execution_cwd;
use super::{LoadedProject, OwnedResource, TaskWorkspace, TaskWorkspacePhase};
use crate::project::{ProjectDefinition, ProjectError, ProjectPrivateState};
use crate::service_supervisor::{self, ServiceSupervisorLease};

const LEASE_WAIT: Duration = Duration::from_secs(10);
const LEASE_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskServiceControl {
    pub service_id: String,
    pub instance_id: String,
    pub directory: PathBuf,
    pub lease_path: PathBuf,
    pub start_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskServiceInvocation {
    pub control: TaskServiceControl,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

pub trait TaskServiceRuntime {
    fn ensure_waiting(&self, invocation: &TaskServiceInvocation) -> Result<(), ProjectError>;
    fn verify(&self, control: &TaskServiceControl) -> Result<(), ProjectError>;
    fn release_start(&self, control: &TaskServiceControl) -> Result<(), ProjectError>;
    fn stop(&self, control: &TaskServiceControl) -> Result<(), ProjectError>;
}

pub struct SystemTaskServiceRuntime {
    executable: PathBuf,
    children: Mutex<BTreeMap<String, Child>>,
}

impl SystemTaskServiceRuntime {
    pub fn current() -> Result<Self, ProjectError> {
        let executable =
            std::env::current_exe().map_err(|error| service_io("executable", &error))?;
        Ok(Self {
            executable,
            children: Mutex::new(BTreeMap::new()),
        })
    }

    fn children(&self) -> Result<MutexGuard<'_, BTreeMap<String, Child>>, ProjectError> {
        self.children.lock().map_err(|_| {
            ProjectError::new(
                "task_service_runtime_unavailable",
                "task service process state is unavailable",
            )
        })
    }

    fn existing_lease(
        &self,
        control: &TaskServiceControl,
    ) -> Result<Option<ServiceSupervisorLease>, ProjectError> {
        match service_supervisor::read_lease(&control.lease_path) {
            Ok(lease) => {
                require_matching_lease(control, &lease)?;
                Ok(Some(lease))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(service_io("lease read", &error)),
        }
    }

    fn remove_stale_control(&self, control: &TaskServiceControl) -> Result<(), ProjectError> {
        for path in [&control.start_path, &control.lease_path] {
            match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    return Err(ProjectError::new(
                        "task_service_control_unsafe",
                        "task service control path is not a regular file",
                    ));
                }
                Ok(_) => fs::remove_file(path)
                    .map_err(|error| service_io("stale control cleanup", &error))?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(service_io("control metadata", &error)),
            }
        }
        Ok(())
    }

    fn wait_for_lease(
        &self,
        invocation: &TaskServiceInvocation,
        child: &mut Child,
    ) -> Result<ServiceSupervisorLease, ProjectError> {
        let deadline = Instant::now() + LEASE_WAIT;
        loop {
            match service_supervisor::read_lease(&invocation.control.lease_path) {
                Ok(lease) => {
                    require_matching_lease(&invocation.control, &lease)?;
                    if lease.pid != child.id()
                        || !crate::platform::service_process_matches(
                            lease.pid,
                            lease.started_at_unix_millis,
                        )
                    {
                        return Err(ProjectError::new(
                            "task_service_identity_mismatch",
                            "task service supervisor lease does not match the launched process",
                        ));
                    }
                    return Ok(lease);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(service_io("lease read", &error)),
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|error| service_io("supervisor status", &error))?
            {
                return Err(ProjectError::new(
                    "task_service_supervisor_exited",
                    format!("task service supervisor exited before leasing ({status})"),
                ));
            }
            if Instant::now() >= deadline {
                return Err(ProjectError::new(
                    "task_service_supervisor_timeout",
                    "task service supervisor did not publish its lease",
                ));
            }
            std::thread::sleep(LEASE_POLL);
        }
    }
}

impl TaskServiceRuntime for SystemTaskServiceRuntime {
    fn ensure_waiting(&self, invocation: &TaskServiceInvocation) -> Result<(), ProjectError> {
        ensure_private_directory(&invocation.control.directory)?;
        let log_directory = invocation.stdout_path.parent().ok_or_else(|| {
            ProjectError::new("task_service_log_unsafe", "service log has no parent")
        })?;
        ensure_private_directory(log_directory)?;

        if let Some(lease) = self.existing_lease(&invocation.control)? {
            if crate::platform::service_process_matches(lease.pid, lease.started_at_unix_millis) {
                return Ok(());
            }
            self.remove_stale_control(&invocation.control)?;
        } else {
            self.remove_stale_control(&invocation.control)?;
        }

        let stdout = open_private_log(&invocation.stdout_path)?;
        let stderr = open_private_log(&invocation.stderr_path)?;
        let mut command = service_supervisor::command(
            &self.executable,
            &invocation.control.instance_id,
            &invocation.control.lease_path,
            &invocation.control.start_path,
            &invocation.argv,
        )
        .map_err(|error| service_io("supervisor command", &error))?;
        command
            .current_dir(&invocation.cwd)
            .envs(&invocation.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let mut child = command
            .spawn()
            .map_err(|error| service_io("supervisor spawn", &error))?;
        let lease = match self.wait_for_lease(invocation, &mut child) {
            Ok(lease) => lease,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let mut children = self.children()?;
        if children.contains_key(&invocation.control.instance_id) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProjectError::new(
                "task_service_runtime_collision",
                "task service runtime already owns this process instance",
            ));
        }
        children.insert(invocation.control.instance_id.clone(), child);
        require_matching_lease(&invocation.control, &lease)
    }

    fn verify(&self, control: &TaskServiceControl) -> Result<(), ProjectError> {
        let mut children = self.children()?;
        let child_exited = if let Some(child) = children.get_mut(&control.instance_id) {
            child
                .try_wait()
                .map_err(|error| service_io("supervisor status", &error))?
                .is_some()
        } else {
            false
        };
        if child_exited {
            children.remove(&control.instance_id);
            return Err(ProjectError::new(
                "task_service_not_running",
                format!("task service '{}' is not running", control.service_id),
            ));
        }
        drop(children);
        let lease = self.existing_lease(control)?.ok_or_else(|| {
            ProjectError::new(
                "task_service_lease_missing",
                format!("task service '{}' lease is missing", control.service_id),
            )
        })?;
        if crate::platform::service_process_matches(lease.pid, lease.started_at_unix_millis) {
            Ok(())
        } else {
            Err(ProjectError::new(
                "task_service_not_running",
                format!("task service '{}' is not running", control.service_id),
            ))
        }
    }

    fn release_start(&self, control: &TaskServiceControl) -> Result<(), ProjectError> {
        self.verify(control)?;
        service_supervisor::write_start_signal(&control.start_path, &control.instance_id)
            .map_err(|error| service_io("start signal", &error))
    }

    fn stop(&self, control: &TaskServiceControl) -> Result<(), ProjectError> {
        let Some(lease) = self.existing_lease(control)? else {
            if let Some(mut child) = self.children()?.remove(&control.instance_id) {
                if child
                    .try_wait()
                    .map_err(|error| service_io("supervisor status", &error))?
                    .is_none()
                {
                    child
                        .kill()
                        .map_err(|error| service_io("supervisor termination", &error))?;
                }
                child
                    .wait()
                    .map_err(|error| service_io("supervisor reap", &error))?;
            }
            self.remove_stale_control(control)?;
            return Ok(());
        };
        crate::platform::terminate_service_process(lease.pid, lease.started_at_unix_millis)
            .map_err(|error| service_io("process termination", &error))?;
        if let Some(mut child) = self.children()?.remove(&control.instance_id) {
            child
                .wait()
                .map_err(|error| service_io("supervisor reap", &error))?;
        }
        self.remove_stale_control(control)
    }
}

impl TaskWorkspaceProvisioner<'_> {
    pub fn start_services(
        &self,
        definition: &ProjectDefinition,
        private_state: &ProjectPrivateState,
        project: &LoadedProject,
        task_id: &str,
        runtime: &dyn TaskServiceRuntime,
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
                "task services can start only after workspace provisioning",
            ));
        }
        verify_provisioned_task(&task)?;
        self.verify_runtime_ports(&task)?;
        if !project
            .manifest
            .services
            .iter()
            .any(|service| !service.isolation.compose)
        {
            return Ok(task);
        }

        for service in project
            .manifest
            .services
            .iter()
            .filter(|service| !service.isolation.compose)
        {
            let invocation = service_invocation(&task, service)?;
            let resource = service_resource(&invocation.control);
            if task.resource_is_owned(&resource) {
                match runtime.verify(&invocation.control) {
                    Ok(()) => {}
                    Err(error) if service_can_restart_after(&error) => {
                        let stop_control = invocation.control.clone();
                        self.ensure_released(
                            &mut task,
                            resource.clone(),
                            || Ok(()),
                            || runtime.stop(&stop_control),
                        )?;
                    }
                    Err(error) => {
                        self.mark_service_attention(&mut task)?;
                        return Err(error);
                    }
                }
            }
            self.ensure_acquired(
                &mut task,
                resource,
                || runtime.verify(&invocation.control),
                || runtime.ensure_waiting(&invocation),
            )?;
            if let Err(error) = runtime.release_start(&invocation.control) {
                self.mark_service_attention(&mut task)?;
                return Err(error);
            }
        }
        if task.phase != TaskWorkspacePhase::Running {
            self.transition_phase(&mut task, TaskWorkspacePhase::Running)?;
        }
        Ok(task)
    }

    pub fn stop_services(
        &self,
        task_id: &str,
        runtime: &dyn TaskServiceRuntime,
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
                "task services can stop only from a provisioned workspace",
            ));
        }
        let service_ids = task
            .runtime
            .declared_services
            .iter()
            .rev()
            .cloned()
            .collect::<Vec<_>>();
        for service_id in service_ids {
            let control = service_control(&task, &service_id);
            let resource = service_resource(&control);
            if task.resource_is_owned(&resource) {
                let stop_control = control.clone();
                self.ensure_released(
                    &mut task,
                    resource,
                    || Ok(()),
                    || runtime.stop(&stop_control),
                )?;
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

    fn mark_service_attention(&self, task: &mut TaskWorkspace) -> Result<(), ProjectError> {
        if task.phase != TaskWorkspacePhase::NeedsAttention {
            self.transition_phase(task, TaskWorkspacePhase::NeedsAttention)?;
        }
        Ok(())
    }
}

fn service_invocation(
    task: &TaskWorkspace,
    service: &crate::project::model::ProjectService,
) -> Result<TaskServiceInvocation, ProjectError> {
    let control = service_control(task, &service.id);
    let cwd = resolve_execution_cwd(task, service.repository.as_deref(), service.cwd.as_deref())?;
    let mut environment = task.runtime.command_environment();
    environment.extend(service.environment.clone());
    let log_directory = task.runtime.data.join("logs").join("services");
    Ok(TaskServiceInvocation {
        control,
        argv: service.argv.clone(),
        cwd,
        environment,
        stdout_path: log_directory.join(format!("{}.stdout.log", service.id)),
        stderr_path: log_directory.join(format!("{}.stderr.log", service.id)),
    })
}

fn service_control(task: &TaskWorkspace, service_id: &str) -> TaskServiceControl {
    let directory = task
        .runtime
        .root
        .join("control")
        .join("services")
        .join(service_id);
    TaskServiceControl {
        service_id: service_id.to_string(),
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

fn require_matching_lease(
    control: &TaskServiceControl,
    lease: &ServiceSupervisorLease,
) -> Result<(), ProjectError> {
    if lease.instance_id == control.instance_id {
        Ok(())
    } else {
        Err(ProjectError::new(
            "task_service_identity_mismatch",
            format!(
                "task service '{}' lease belongs to another instance",
                control.service_id
            ),
        ))
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), ProjectError> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            ensure_private_directory(parent)?;
        }
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(ProjectError::new(
                "task_service_directory_unsafe",
                "task service directory is not a private directory",
            ))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::platform::create_private_directory(path)
                .map_err(|error| service_io("directory creation", &error))
        }
        Err(error) => Err(service_io("directory metadata", &error)),
    }
}

fn open_private_log(path: &Path) -> Result<File, ProjectError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(ProjectError::new(
                "task_service_log_unsafe",
                "task service log path is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(service_io("log metadata", &error)),
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options
        .open(path)
        .map_err(|error| service_io("log open", &error))
}

fn service_io(operation: &str, error: &std::io::Error) -> ProjectError {
    ProjectError::new(
        "task_service_io_failed",
        format!("task service {operation} failed ({:?})", error.kind()),
    )
}

fn service_can_restart_after(error: &ProjectError) -> bool {
    matches!(
        error.code,
        "task_service_not_running" | "task_service_lease_missing"
    )
}
