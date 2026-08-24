use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::provision::{
    require_matching_definition, verify_provisioned_task, TaskWorkspaceProvisioner,
};
use super::{LoadedProject, TaskWorkspace, TaskWorkspacePhase};
use crate::project::{ProjectDefinition, ProjectError, ProjectPrivateState};

const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCommandKind {
    Setup,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCommandInvocation {
    pub id: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCommandResult {
    pub id: String,
    pub cwd: PathBuf,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl TaskWorkspaceProvisioner<'_> {
    pub fn run_command(
        &self,
        definition: &ProjectDefinition,
        private_state: &ProjectPrivateState,
        project: &LoadedProject,
        task_id: &str,
        kind: TaskCommandKind,
        command_id: &str,
    ) -> Result<TaskCommandResult, ProjectError> {
        require_matching_definition(definition, project)?;
        private_state.require_execution_trust(definition)?;
        let _operation_lock = self.states().lock_task_operations(task_id)?;
        let task = self.states().load(task_id)?;
        let invocation = self.resolve_command(project, &task, kind, command_id)?;
        run_invocation(invocation)
    }

    pub(super) fn prepare_command(
        &self,
        definition: &ProjectDefinition,
        private_state: &ProjectPrivateState,
        project: &LoadedProject,
        task_id: &str,
        kind: TaskCommandKind,
        command_id: &str,
    ) -> Result<TaskCommandInvocation, ProjectError> {
        require_matching_definition(definition, project)?;
        private_state.require_execution_trust(definition)?;
        let _operation_lock = self.states().lock_task_operations(task_id)?;
        let task = self.states().load(task_id)?;
        self.resolve_command(project, &task, kind, command_id)
    }

    fn resolve_command(
        &self,
        project: &LoadedProject,
        task: &TaskWorkspace,
        kind: TaskCommandKind,
        command_id: &str,
    ) -> Result<TaskCommandInvocation, ProjectError> {
        task.validate(project)?;
        if !matches!(
            task.phase,
            TaskWorkspacePhase::Ready | TaskWorkspacePhase::Stopped
        ) {
            return Err(ProjectError::new(
                "task_workspace_not_executable",
                "project commands can run only after provisioning and while the task is stopped",
            ));
        }
        verify_provisioned_task(task)?;
        self.verify_runtime_ports(task)?;
        let commands = match kind {
            TaskCommandKind::Setup => &project.manifest.setup,
            TaskCommandKind::Test => &project.manifest.tests,
        };
        let command = commands
            .iter()
            .find(|command| command.id == command_id)
            .ok_or_else(|| {
                ProjectError::new(
                    "unknown_task_command",
                    format!("project has no {} command '{command_id}'", kind.label()),
                )
            })?;
        let cwd =
            resolve_execution_cwd(task, command.repository.as_deref(), command.cwd.as_deref())?;
        let mut environment = task.runtime.command_environment();
        environment.extend(command.environment.clone());
        Ok(TaskCommandInvocation {
            id: command.id.clone(),
            argv: command.argv.clone(),
            cwd,
            environment,
        })
    }

    pub(super) fn verify_runtime_ports(&self, task: &TaskWorkspace) -> Result<(), ProjectError> {
        if task.runtime.declared_ports.is_empty() {
            return Ok(());
        }
        if task.runtime.ports.len() != task.runtime.declared_ports.len()
            || !task
                .runtime
                .declared_ports
                .iter()
                .all(|name| task.runtime.ports.contains_key(name))
        {
            return Err(ProjectError::new(
                "task_ports_not_ready",
                "every declared task port must be reserved before project commands run",
            ));
        }
        let broker = self.port_broker()?;
        for (name, port) in &task.runtime.ports {
            broker.verify_exact(&task.runtime.namespace, name, *port)?;
        }
        Ok(())
    }
}

impl TaskCommandKind {
    fn label(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Test => "test",
        }
    }
}

pub(super) fn resolve_execution_cwd(
    task: &TaskWorkspace,
    repository_id: Option<&str>,
    cwd: Option<&Path>,
) -> Result<PathBuf, ProjectError> {
    let base = if let Some(repository_id) = repository_id {
        task.repositories
            .get(repository_id)
            .and_then(|repository| repository.worktree.as_ref())
            .map(|worktree| worktree.checkout_path.clone())
            .ok_or_else(|| {
                ProjectError::new(
                    "task_command_repository_not_ready",
                    format!("repository '{repository_id}' has no task checkout"),
                )
            })?
    } else {
        task.root.join("repositories")
    };
    let requested = cwd.map_or_else(|| base.clone(), |cwd| base.join(cwd));
    canonical_directory_within(&requested, &base)
}

fn canonical_directory_within(path: &Path, boundary: &Path) -> Result<PathBuf, ProjectError> {
    let boundary = std::fs::canonicalize(boundary).map_err(|_| {
        ProjectError::new(
            "task_command_cwd_missing",
            "task command base directory is missing",
        )
    })?;
    let path = std::fs::canonicalize(path).map_err(|_| {
        ProjectError::new(
            "task_command_cwd_missing",
            "task command working directory is missing",
        )
    })?;
    if path.starts_with(&boundary) && path.is_dir() {
        Ok(path)
    } else {
        Err(ProjectError::new(
            "task_command_cwd_escape",
            "task command working directory escapes its isolated repository boundary",
        ))
    }
}

fn run_invocation(invocation: TaskCommandInvocation) -> Result<TaskCommandResult, ProjectError> {
    let Some((program, arguments)) = invocation.argv.split_first() else {
        return Err(ProjectError::new(
            "task_command_invalid",
            "task command has no program to execute",
        ));
    };
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(&invocation.cwd)
        .envs(&invocation.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        ProjectError::new(
            "task_command_spawn_failed",
            format!("task command could not start ({:?})", error.kind()),
        )
    })?;
    let Some(stdout) = child.stdout.take() else {
        stop_child(&mut child);
        return Err(ProjectError::new(
            "task_command_output_failed",
            "task command stdout was not captured",
        ));
    };
    let Some(stderr) = child.stderr.take() else {
        stop_child(&mut child);
        return Err(ProjectError::new(
            "task_command_output_failed",
            "task command stderr was not captured",
        ));
    };
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    let status = child.wait();
    let stdout = join_reader(stdout_reader);
    let stderr = join_reader(stderr_reader);
    let status = status.map_err(|error| {
        ProjectError::new(
            "task_command_wait_failed",
            format!("task command status was unavailable ({:?})", error.kind()),
        )
    })?;
    let (stdout, stdout_truncated) = stdout?;
    let (stderr, stderr_truncated) = stderr?;
    Ok(TaskCommandResult {
        id: invocation.id,
        cwd: invocation.cwd,
        exit_code: status.code(),
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
        stderr_truncated,
    })
}

fn stop_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn join_reader(
    reader: std::thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
) -> Result<(Vec<u8>, bool), ProjectError> {
    reader
        .join()
        .map_err(|_| {
            ProjectError::new(
                "task_command_output_failed",
                "task command output reader stopped unexpectedly",
            )
        })?
        .map_err(|error| {
            ProjectError::new(
                "task_command_output_failed",
                format!("task command output could not be read ({:?})", error.kind()),
            )
        })
}

fn read_bounded(mut reader: impl Read) -> io::Result<(Vec<u8>, bool)> {
    let mut captured = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(captured.len());
        let keep = remaining.min(read);
        captured.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok((captured, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_drains_but_caps_output() {
        let bytes = vec![b'x'; MAX_COMMAND_OUTPUT_BYTES + 17];
        let (captured, truncated) = read_bounded(bytes.as_slice()).unwrap();
        assert_eq!(captured.len(), MAX_COMMAND_OUTPUT_BYTES);
        assert!(truncated);
    }
}
