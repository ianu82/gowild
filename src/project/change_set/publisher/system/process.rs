use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};

use crate::project::ProjectError;

const MAX_COMMAND_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_COMMAND_ERROR_BYTES: usize = 64 * 1024;

pub(super) fn run_checked<I, S>(
    program: &Path,
    cwd: &Path,
    args: I,
    stdin: Option<&[u8]>,
    operation: &'static str,
    environment: &BTreeMap<OsString, OsString>,
) -> Result<Vec<u8>, ProjectError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = crate::noninteractive_process::command(program);
    command
        .args(args)
        .current_dir(cwd)
        .envs(environment)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|_| command_failed(operation))?;
    if let Some(input) = stdin {
        let Some(mut child_stdin) = child.stdin.take() else {
            stop_child(&mut child);
            return Err(command_failed(operation));
        };
        if child_stdin.write_all(input).is_err() {
            stop_child(&mut child);
            return Err(command_failed(operation));
        }
    }
    let Some(stdout) = child.stdout.take() else {
        stop_child(&mut child);
        return Err(command_failed(operation));
    };
    let Some(stderr) = child.stderr.take() else {
        stop_child(&mut child);
        return Err(command_failed(operation));
    };
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout, MAX_COMMAND_OUTPUT_BYTES));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr, MAX_COMMAND_ERROR_BYTES));
    let status = child.wait().map_err(|_| command_failed(operation))?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| command_failed(operation))?
        .map_err(|_| command_failed(operation))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| command_failed(operation))?
        .map_err(|_| command_failed(operation))?;
    if stdout.1 || stderr.1 {
        return Err(ProjectError::new(
            "task_change_set_publication_output_too_large",
            format!("{operation} exceeded the bounded command-output limit"),
        ));
    }
    if status.success() {
        Ok(stdout.0)
    } else {
        Err(command_failed(operation))
    }
}

fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok((output, false));
        }
        let remaining = limit.saturating_sub(output.len());
        if read > remaining {
            output.extend_from_slice(&buffer[..remaining]);
            return Ok((output, true));
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn command_failed(operation: &'static str) -> ProjectError {
    ProjectError::new(
        "task_change_set_publication_command_failed",
        format!("{operation} failed noninteractively"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_stops_at_the_limit() {
        let (output, overflowed) = read_bounded(b"12345".as_slice(), 4).unwrap();
        assert_eq!(output, b"1234");
        assert!(overflowed);
    }
}
