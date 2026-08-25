use std::io::{self, Read};
use std::path::Path;
use std::process::Stdio;

use sha2::{Digest, Sha256};

use super::git_invalid_output;
use crate::project::change_set::DiffSummary;
use crate::project::ProjectError;

const MAX_GIT_FACT_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIFF_BYTES: u64 = 64 * 1024 * 1024;
const DIFF_DISPLAY_BYTES: u64 = 1024 * 1024;
const MAX_GIT_ERROR_BYTES: usize = 64 * 1024;

pub(super) fn diff_digest(
    repository_id: &str,
    checkout: &Path,
    base_commit: &str,
) -> Result<DiffSummary, ProjectError> {
    let mut command = git_command(checkout);
    command.args([
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--unified=3",
        base_commit,
        "--",
    ]);
    let mut child = command
        .spawn()
        .map_err(|_| git_unavailable(repository_id))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| git_unavailable(repository_id))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| git_unavailable(repository_id))?;
    let stdout_reader = std::thread::spawn(move || digest_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr, MAX_GIT_ERROR_BYTES));
    let status = child.wait().map_err(|_| git_unavailable(repository_id))?;
    let digest = stdout_reader
        .join()
        .map_err(|_| git_unavailable(repository_id))?
        .map_err(|error| git_output_error(repository_id, &error))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| git_unavailable(repository_id))?
        .map_err(|error| git_output_error(repository_id, &error))?;
    if stderr.1 {
        return Err(git_output_too_large(repository_id));
    }
    if !status.success() {
        return Err(git_command_failed(repository_id));
    }
    Ok(DiffSummary {
        sha256: hex_digest(&digest.0),
        bytes: digest.1,
        truncated: digest.1 > DIFF_DISPLAY_BYTES,
    })
}

pub(super) fn git_text(
    repository_id: &str,
    checkout: &Path,
    args: &[&str],
) -> Result<String, ProjectError> {
    let output = git_bytes(repository_id, checkout, args)?;
    String::from_utf8(output)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| git_invalid_output(repository_id))
}

pub(super) fn git_bytes(
    repository_id: &str,
    checkout: &Path,
    args: &[&str],
) -> Result<Vec<u8>, ProjectError> {
    let mut child = git_command(checkout)
        .args(args)
        .spawn()
        .map_err(|_| git_unavailable(repository_id))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| git_unavailable(repository_id))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| git_unavailable(repository_id))?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout, MAX_GIT_FACT_BYTES));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr, MAX_GIT_ERROR_BYTES));
    let status = child.wait().map_err(|_| git_unavailable(repository_id))?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| git_unavailable(repository_id))?
        .map_err(|error| git_output_error(repository_id, &error))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| git_unavailable(repository_id))?
        .map_err(|error| git_output_error(repository_id, &error))?;
    if stdout.1 || stderr.1 {
        return Err(git_output_too_large(repository_id));
    }
    if status.success() {
        Ok(stdout.0)
    } else {
        Err(git_command_failed(repository_id))
    }
}

fn git_command(checkout: &Path) -> std::process::Command {
    let mut command = crate::noninteractive_process::command("git");
    command
        .arg("-C")
        .arg(checkout)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
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

fn digest_bounded(mut reader: impl Read) -> io::Result<(Vec<u8>, u64)> {
    digest_with_limit(&mut reader, MAX_DIFF_BYTES)
}

fn digest_with_limit(mut reader: impl Read, limit: u64) -> io::Result<(Vec<u8>, u64)> {
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok((hasher.finalize().to_vec(), bytes));
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("diff byte count overflow"))?;
        if bytes > limit {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "diff exceeds change-set limit",
            ));
        }
        hasher.update(&buffer[..read]);
    }
}

fn hex_digest(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn git_unavailable(repository_id: &str) -> ProjectError {
    ProjectError::new(
        "task_change_set_git_unavailable",
        format!("Git became unavailable for repository '{repository_id}'"),
    )
}

fn git_command_failed(repository_id: &str) -> ProjectError {
    ProjectError::new(
        "task_change_set_git_command_failed",
        format!("Git could not inspect repository '{repository_id}'"),
    )
}

fn git_output_too_large(repository_id: &str) -> ProjectError {
    ProjectError::new(
        "task_change_set_git_output_too_large",
        format!("repository '{repository_id}' exceeds the bounded Git output limit"),
    )
}

fn git_output_error(repository_id: &str, error: &io::Error) -> ProjectError {
    if error.kind() == io::ErrorKind::FileTooLarge {
        git_output_too_large(repository_id)
    } else {
        git_unavailable(repository_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_fail_closed_at_their_output_limits() {
        let (captured, overflowed) = read_bounded(b"12345".as_slice(), 4).unwrap();
        assert_eq!(captured, b"1234");
        assert!(overflowed);

        let error = digest_with_limit(b"12345".as_slice(), 4).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
    }
}
