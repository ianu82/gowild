use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::repository::ensure_private_directory_chain;
use super::runtime_commands::{resolve_execution_cwd, run_invocation, TaskCommandInvocation};
use super::TaskWorkspace;
use crate::project::model::{validate_compose_command, ProjectService};
use crate::project::private_state::write::{atomic_owner_only_write, PrivateWriteMode};
use crate::project::ProjectError;

const COMPOSE_CONTROL_VERSION: u32 = 1;
const MAX_COMPOSE_CONTROL_BYTES: u64 = 64 * 1024;
const DEFAULT_COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskComposeControl {
    pub project_name: String,
    pub task_root: PathBuf,
    pub descriptor_path: PathBuf,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskComposeInvocation {
    pub service_id: String,
    pub control: TaskComposeControl,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskComposeDescriptor {
    schema_version: u32,
    project_name: String,
    cwd: PathBuf,
    command_prefix: Vec<String>,
}

pub trait TaskComposeRuntime {
    fn ensure_up(&self, invocation: &TaskComposeInvocation) -> Result<(), ProjectError>;
    fn verify(&self, control: &TaskComposeControl) -> Result<(), ProjectError>;
    fn down(&self, control: &TaskComposeControl) -> Result<(), ProjectError>;
}

#[derive(Debug, Default)]
pub struct SystemTaskComposeRuntime;

impl TaskComposeRuntime for SystemTaskComposeRuntime {
    fn ensure_up(&self, invocation: &TaskComposeInvocation) -> Result<(), ProjectError> {
        ensure_compose_descriptor(invocation)?;
        match self.verify(&invocation.control) {
            Ok(()) => return Ok(()),
            Err(error) if error.code == "task_compose_not_running" => {}
            Err(error) => return Err(error),
        }
        let result = run_compose_command(
            "up",
            invocation.argv.clone(),
            invocation.cwd.clone(),
            invocation.environment.clone(),
        )?;
        require_compose_success("up", result.success, result.exit_code)?;
        self.verify(&invocation.control)
    }

    fn verify(&self, control: &TaskComposeControl) -> Result<(), ProjectError> {
        let descriptor = read_compose_descriptor(control)?;
        let result = run_compose_command(
            "ps",
            compose_command(&descriptor.command_prefix, &["ps", "--all", "--quiet"]),
            descriptor.cwd,
            control.environment.clone(),
        )?;
        require_compose_success("ps", result.success, result.exit_code)?;
        if result.stdout.trim().is_empty() {
            Err(ProjectError::new(
                "task_compose_not_running",
                "the task Compose stack has no containers",
            ))
        } else {
            Ok(())
        }
    }

    fn down(&self, control: &TaskComposeControl) -> Result<(), ProjectError> {
        let descriptor = read_compose_descriptor(control)?;
        let result = run_compose_command(
            "down",
            compose_command(&descriptor.command_prefix, &["down", "--remove-orphans"]),
            descriptor.cwd,
            control.environment.clone(),
        )?;
        require_compose_success("down", result.success, result.exit_code)?;
        match self.verify(control) {
            Err(error) if error.code == "task_compose_not_running" => Ok(()),
            Ok(()) => Err(ProjectError::new(
                "task_compose_still_running",
                "the task Compose stack still has containers after shutdown",
            )),
            Err(error) => Err(error),
        }
    }
}

pub(super) fn prepare_compose_invocation(
    task: &TaskWorkspace,
    service: &ProjectService,
) -> Result<TaskComposeInvocation, ProjectError> {
    let up_index = validate_compose_command(&service.argv).map_err(compose_manifest_error)?;
    let cwd = resolve_execution_cwd(task, service.repository.as_deref(), service.cwd.as_deref())?;
    let mut command_prefix = service.argv[..up_index].to_vec();
    let explicit_files = validate_compose_inputs(&command_prefix, &cwd)?;
    if explicit_files == 0 {
        let filename = default_compose_file(&cwd)?;
        command_prefix.push("--file".into());
        command_prefix.push(filename);
    }
    validate_pinned_compose_prefix(&command_prefix, &cwd)?;
    let mut argv = command_prefix.clone();
    argv.extend_from_slice(&service.argv[up_index..]);
    let control_environment = task.runtime.command_environment();
    let mut environment = control_environment.clone();
    environment.extend(service.environment.clone());
    let descriptor_path = task
        .runtime
        .root
        .join("control")
        .join("compose")
        .join("stack.json");
    Ok(TaskComposeInvocation {
        service_id: service.id.clone(),
        control: TaskComposeControl {
            project_name: task.runtime.compose_project.clone(),
            task_root: task.root.clone(),
            descriptor_path,
            environment: control_environment,
        },
        argv,
        cwd,
        environment,
    })
}

pub(super) fn compose_control(task: &TaskWorkspace) -> TaskComposeControl {
    TaskComposeControl {
        project_name: task.runtime.compose_project.clone(),
        task_root: task.root.clone(),
        descriptor_path: task
            .runtime
            .root
            .join("control")
            .join("compose")
            .join("stack.json"),
        environment: task.runtime.command_environment(),
    }
}

fn ensure_compose_descriptor(invocation: &TaskComposeInvocation) -> Result<(), ProjectError> {
    validate_control(&invocation.control)?;
    let descriptor = TaskComposeDescriptor {
        schema_version: COMPOSE_CONTROL_VERSION,
        project_name: invocation.control.project_name.clone(),
        cwd: invocation.cwd.clone(),
        command_prefix: command_prefix(&invocation.argv)?.to_vec(),
    };
    validate_descriptor(&invocation.control, &descriptor)?;
    match read_compose_descriptor(&invocation.control) {
        Ok(existing) if existing == descriptor => return Ok(()),
        Ok(_) => {
            return Err(ProjectError::new(
                "task_compose_control_mismatch",
                "the task Compose control record belongs to another command",
            ))
        }
        Err(error) if error.code == "task_compose_control_missing" => {}
        Err(error) => return Err(error),
    }
    let parent = invocation
        .control
        .descriptor_path
        .parent()
        .ok_or_else(|| compose_control_error("control path has no parent"))?;
    let _ = ensure_private_directory_chain(parent)?;
    let bytes = serde_json::to_vec_pretty(&descriptor)
        .map_err(|_| compose_control_error("control record could not be serialized"))?;
    match atomic_owner_only_write(
        &invocation.control.descriptor_path,
        &bytes,
        PrivateWriteMode::CreateNew,
    ) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if read_compose_descriptor(&invocation.control)? == descriptor {
                Ok(())
            } else {
                Err(ProjectError::new(
                    "task_compose_control_mismatch",
                    "the task Compose control record changed during creation",
                ))
            }
        }
        Err(error) => Err(compose_io("control write", &error)),
    }
}

fn read_compose_descriptor(
    control: &TaskComposeControl,
) -> Result<TaskComposeDescriptor, ProjectError> {
    validate_control(control)?;
    let metadata = fs::symlink_metadata(&control.descriptor_path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ProjectError::new(
                "task_compose_control_missing",
                "the task Compose control record is missing",
            )
        } else {
            compose_io("control metadata", &error)
        }
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_COMPOSE_CONTROL_BYTES
    {
        return Err(compose_control_error(
            "control record is not a bounded regular file",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(&control.descriptor_path)
        .map_err(|error| compose_io("control open", &error))?;
    let mut bytes = Vec::new();
    file.take(MAX_COMPOSE_CONTROL_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| compose_io("control read", &error))?;
    if bytes.len() as u64 > MAX_COMPOSE_CONTROL_BYTES {
        return Err(compose_control_error(
            "control record exceeds its size limit",
        ));
    }
    let descriptor = serde_json::from_slice::<TaskComposeDescriptor>(&bytes)
        .map_err(|_| compose_control_error("control record is malformed"))?;
    validate_descriptor(control, &descriptor)?;
    Ok(descriptor)
}

fn validate_control(control: &TaskComposeControl) -> Result<(), ProjectError> {
    let expected = control
        .task_root
        .join("runtime")
        .join("control")
        .join("compose")
        .join("stack.json");
    if control.descriptor_path != expected {
        return Err(compose_control_error(
            "control record escapes the task runtime boundary",
        ));
    }
    Ok(())
}

fn validate_descriptor(
    control: &TaskComposeControl,
    descriptor: &TaskComposeDescriptor,
) -> Result<(), ProjectError> {
    if descriptor.schema_version != COMPOSE_CONTROL_VERSION
        || descriptor.project_name != control.project_name
        || !descriptor
            .cwd
            .starts_with(control.task_root.join("repositories"))
        || !descriptor.cwd.is_absolute()
        || descriptor
            .cwd
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(compose_control_error(
            "control record does not match this task boundary",
        ));
    }
    validate_pinned_compose_prefix(&descriptor.command_prefix, &descriptor.cwd)
}

fn command_prefix(argv: &[String]) -> Result<&[String], ProjectError> {
    let up_index = validate_compose_command(argv).map_err(compose_manifest_error)?;
    Ok(&argv[..up_index])
}

fn validate_pinned_compose_prefix(prefix: &[String], cwd: &Path) -> Result<(), ProjectError> {
    let mut command = prefix.to_vec();
    command.extend(["up".into(), "--detach".into()]);
    let up_index = validate_compose_command(&command).map_err(compose_manifest_error)?;
    if up_index != prefix.len() || validate_compose_inputs(prefix, cwd)? == 0 {
        return Err(compose_control_error(
            "Compose control must pin at least one in-task configuration file",
        ));
    }
    Ok(())
}

fn validate_compose_inputs(prefix: &[String], cwd: &Path) -> Result<usize, ProjectError> {
    let mut files = 0usize;
    let mut index = 0usize;
    while index < prefix.len() {
        let argument = &prefix[index];
        if matches!(argument.as_str(), "-f" | "--file" | "--env-file") {
            let value = prefix.get(index + 1).ok_or_else(|| {
                compose_manifest_error("a Compose file option is missing its path")
            })?;
            validate_compose_input_path(cwd, value)?;
            files = files.saturating_add(usize::from(argument != "--env-file"));
            index = index.saturating_add(2);
            continue;
        }
        if let Some(value) = argument
            .strip_prefix("--file=")
            .or_else(|| argument.strip_prefix("--env-file="))
        {
            validate_compose_input_path(cwd, value)?;
            files = files.saturating_add(usize::from(argument.starts_with("--file=")));
        } else if let Some(value) = argument
            .strip_prefix("-f")
            .filter(|value| !value.is_empty())
        {
            validate_compose_input_path(cwd, value)?;
            files = files.saturating_add(1);
        }
        index = index.saturating_add(1);
    }
    Ok(files)
}

fn validate_compose_input_path(cwd: &Path, value: &str) -> Result<(), ProjectError> {
    let path = Path::new(value);
    if value.is_empty()
        || value == "-"
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(compose_manifest_error(
            "Compose file paths must stay inside the task checkout",
        ));
    }
    let requested = cwd.join(path);
    let metadata = fs::symlink_metadata(&requested).map_err(|_| {
        compose_manifest_error("a declared Compose file is missing from the task checkout")
    })?;
    let canonical = fs::canonicalize(&requested).map_err(|_| {
        compose_manifest_error("a declared Compose file could not be resolved safely")
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || !canonical.starts_with(cwd) {
        return Err(compose_manifest_error(
            "Compose files must be regular files inside the task checkout",
        ));
    }
    Ok(())
}

fn default_compose_file(cwd: &Path) -> Result<String, ProjectError> {
    for filename in DEFAULT_COMPOSE_FILES {
        if validate_compose_input_path(cwd, filename).is_ok() {
            return Ok(filename.into());
        }
    }
    Err(ProjectError::new(
        "task_compose_file_missing",
        "no safe Compose file exists in the task working directory; declare one with --file",
    ))
}

fn compose_command(prefix: &[String], arguments: &[&str]) -> Vec<String> {
    let mut command = prefix.to_vec();
    command.extend(arguments.iter().map(|argument| (*argument).to_string()));
    command
}

fn run_compose_command(
    operation: &str,
    argv: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
) -> Result<super::runtime_commands::TaskCommandResult, ProjectError> {
    run_invocation(TaskCommandInvocation {
        id: format!("compose-{operation}"),
        argv,
        cwd,
        environment,
    })
    .map_err(|error| {
        ProjectError::new(
            "task_compose_execution_failed",
            format!("Compose {operation} could not execute ({})", error.code),
        )
    })
}

fn require_compose_success(
    operation: &str,
    success: bool,
    exit_code: Option<i32>,
) -> Result<(), ProjectError> {
    if success {
        Ok(())
    } else {
        Err(ProjectError::new(
            "task_compose_command_failed",
            format!("Compose {operation} failed with exit status {exit_code:?}"),
        ))
    }
}

fn compose_manifest_error(message: &'static str) -> ProjectError {
    ProjectError::new("task_compose_command_invalid", message)
}

fn compose_control_error(message: &'static str) -> ProjectError {
    ProjectError::new("task_compose_control_invalid", message)
}

fn compose_io(operation: &str, error: &io::Error) -> ProjectError {
    ProjectError::new(
        "task_compose_io_failed",
        format!("Compose {operation} failed ({:?})", error.kind()),
    )
}
