use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use super::{ChangeSet, ChangedFile, ChangedFileKind, RepositorySnapshot};
use crate::project::manifest::LoadedProject;
use crate::project::task_workspace::provision::verify_provisioned_task;
use crate::project::task_workspace::repository::TaskWorkspaceRepository;
use crate::project::ProjectError;

mod process;

use process::{diff_digest, git_bytes, git_text};

const MAX_CHANGED_FILES: usize = 50_000;

impl TaskWorkspaceRepository {
    /// Captures one dependency-ordered, read-only view of all task checkouts.
    ///
    /// The task operation lease prevents GoWild lifecycle mutation during the
    /// inspection. Agents may still edit their checkout, so each repository is
    /// an internally consistent Git snapshot rather than a cross-repo lock.
    pub fn inspect_change_set(
        &self,
        project: &LoadedProject,
        task_id: &str,
    ) -> Result<ChangeSet, ProjectError> {
        let _operation_lock = self.lock_task_operations(task_id)?;
        let task = self.load(task_id)?;
        task.validate(project)?;
        verify_provisioned_task(&task)?;
        inspect_task(&task)
    }
}

pub(super) fn inspect_task(
    task: &crate::project::task_workspace::TaskWorkspace,
) -> Result<ChangeSet, ProjectError> {
    let mut change_set = ChangeSet::for_task(task)?;
    for repository_id in &change_set.dependency_order {
        let repository = change_set
            .repositories
            .get_mut(repository_id)
            .ok_or_else(|| {
                ProjectError::new(
                    "task_change_set_repository_mismatch",
                    "change-set dependency order references an unknown repository",
                )
            })?;
        repository.snapshot = inspect_repository(
            repository_id,
            &repository.checkout_path,
            &repository.base_commit,
        )?;
    }
    Ok(change_set)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileAccumulator {
    kind: ChangedFileKind,
    staged: bool,
    worktree: bool,
}

fn inspect_repository(
    repository_id: &str,
    checkout: &Path,
    base_commit: &str,
) -> Result<RepositorySnapshot, ProjectError> {
    verify_checkout(repository_id, checkout)?;
    let head_commit = git_text(repository_id, checkout, &["rev-parse", "--verify", "HEAD"])?;
    let commits_ahead = git_text(
        repository_id,
        checkout,
        &["rev-list", "--count", &format!("{base_commit}..HEAD")],
    )?
    .parse::<u64>()
    .map_err(|_| git_invalid_output(repository_id))?;

    let mut files = BTreeMap::new();
    let base_changes = git_bytes(
        repository_id,
        checkout,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-status",
            "-z",
            "--find-renames",
            "--find-copies",
            base_commit,
            "--",
        ],
    )?;
    parse_name_status(repository_id, &base_changes, &mut files)?;
    let working_changes = git_bytes(
        repository_id,
        checkout,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    parse_worktree_status(repository_id, &working_changes, &mut files)?;

    if files.is_empty() && commits_ahead == 0 {
        return Ok(RepositorySnapshot::Unchanged {
            head_commit,
            commits_ahead,
        });
    }
    if files.len() > MAX_CHANGED_FILES {
        return Err(ProjectError::new(
            "task_change_set_too_many_files",
            format!(
                "repository '{repository_id}' exceeds the {MAX_CHANGED_FILES}-file change-set limit"
            ),
        ));
    }
    let (insertions, deletions) = diff_numstat(repository_id, checkout, base_commit)?;
    let diff = diff_digest(repository_id, checkout, base_commit)?;
    Ok(RepositorySnapshot::Changed {
        head_commit,
        commits_ahead,
        files: files
            .into_iter()
            .map(|(path, change)| ChangedFile {
                path,
                kind: change.kind,
                staged: change.staged,
                worktree: change.worktree,
            })
            .collect(),
        insertions,
        deletions,
        diff,
    })
}

fn verify_checkout(repository_id: &str, checkout: &Path) -> Result<(), ProjectError> {
    let canonical = checkout
        .canonicalize()
        .map_err(|_| git_checkout_mismatch(repository_id))?;
    if canonical != checkout || !canonical.is_dir() {
        return Err(git_checkout_mismatch(repository_id));
    }
    let top_level = PathBuf::from(git_text(
        repository_id,
        checkout,
        &["rev-parse", "--show-toplevel"],
    )?)
    .canonicalize()
    .map_err(|_| git_checkout_mismatch(repository_id))?;
    if top_level == canonical {
        Ok(())
    } else {
        Err(git_checkout_mismatch(repository_id))
    }
}

fn parse_name_status(
    repository_id: &str,
    output: &[u8],
    files: &mut BTreeMap<PathBuf, FileAccumulator>,
) -> Result<(), ProjectError> {
    let fields = nul_fields(output);
    let mut index = 0;
    while index < fields.len() {
        let status = fields[index];
        index += 1;
        let Some(code) = status.first().copied() else {
            return Err(git_invalid_output(repository_id));
        };
        let path = if matches!(code, b'R' | b'C') {
            if index + 1 >= fields.len() {
                return Err(git_invalid_output(repository_id));
            }
            index += 1;
            let destination = fields[index];
            index += 1;
            destination
        } else {
            let Some(path) = fields.get(index).copied() else {
                return Err(git_invalid_output(repository_id));
            };
            index += 1;
            path
        };
        insert_change(
            repository_id,
            files,
            path,
            change_kind(repository_id, code)?,
            false,
            false,
        )?;
    }
    Ok(())
}

fn parse_worktree_status(
    repository_id: &str,
    output: &[u8],
    files: &mut BTreeMap<PathBuf, FileAccumulator>,
) -> Result<(), ProjectError> {
    let fields = nul_fields(output);
    let mut index = 0;
    while index < fields.len() {
        let entry = fields[index];
        index += 1;
        if entry.len() < 4 || entry[2] != b' ' {
            return Err(git_invalid_output(repository_id));
        }
        let x = entry[0];
        let y = entry[1];
        let code = if is_unmerged_status(x, y) {
            b'U'
        } else if x == b'?' {
            b'?'
        } else if y != b' ' {
            y
        } else {
            x
        };
        let path = &entry[3..];
        insert_change(
            repository_id,
            files,
            path,
            change_kind(repository_id, code)?,
            !matches!(x, b' ' | b'?' | b'!'),
            !matches!(y, b' ' | b'!') || x == b'?',
        )?;
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            if index >= fields.len() {
                return Err(git_invalid_output(repository_id));
            }
            index += 1;
        }
    }
    Ok(())
}

fn insert_change(
    repository_id: &str,
    files: &mut BTreeMap<PathBuf, FileAccumulator>,
    path: &[u8],
    kind: ChangedFileKind,
    staged: bool,
    worktree: bool,
) -> Result<(), ProjectError> {
    let path = path_from_git(repository_id, path)?;
    validate_relative_git_path(repository_id, &path)?;
    files
        .entry(path)
        .and_modify(|change| {
            change.staged |= staged;
            change.worktree |= worktree;
            if matches!(kind, ChangedFileKind::Deleted | ChangedFileKind::Unmerged)
                || matches!(change.kind, ChangedFileKind::Modified)
            {
                change.kind = kind;
            }
        })
        .or_insert(FileAccumulator {
            kind,
            staged,
            worktree,
        });
    Ok(())
}

fn change_kind(repository_id: &str, code: u8) -> Result<ChangedFileKind, ProjectError> {
    match code {
        b'A' => Ok(ChangedFileKind::Added),
        b'M' | b'T' | b'm' => Ok(ChangedFileKind::Modified),
        b'D' => Ok(ChangedFileKind::Deleted),
        b'R' => Ok(ChangedFileKind::Renamed),
        b'C' => Ok(ChangedFileKind::Copied),
        b'U' => Ok(ChangedFileKind::Unmerged),
        b'?' => Ok(ChangedFileKind::Untracked),
        _ => Err(git_invalid_output(repository_id)),
    }
}

fn is_unmerged_status(x: u8, y: u8) -> bool {
    matches!(
        (x, y),
        (b'D', b'D')
            | (b'A', b'U')
            | (b'U', b'D')
            | (b'U', b'A')
            | (b'D', b'U')
            | (b'A', b'A')
            | (b'U', b'U')
    )
}

fn diff_numstat(
    repository_id: &str,
    checkout: &Path,
    base_commit: &str,
) -> Result<(u64, u64), ProjectError> {
    let output = git_bytes(
        repository_id,
        checkout,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            "-z",
            base_commit,
            "--",
        ],
    )?;
    let mut insertions = 0_u64;
    let mut deletions = 0_u64;
    for field in nul_fields(&output) {
        let mut columns = field.splitn(3, |byte| *byte == b'\t');
        let Some(added) = columns.next() else {
            continue;
        };
        let Some(deleted) = columns.next() else {
            continue;
        };
        insertions = insertions
            .checked_add(parse_numstat(repository_id, added)?)
            .ok_or_else(|| git_invalid_output(repository_id))?;
        deletions = deletions
            .checked_add(parse_numstat(repository_id, deleted)?)
            .ok_or_else(|| git_invalid_output(repository_id))?;
    }
    Ok((insertions, deletions))
}

fn parse_numstat(repository_id: &str, value: &[u8]) -> Result<u64, ProjectError> {
    if value == b"-" {
        return Ok(0);
    }
    std::str::from_utf8(value)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| git_invalid_output(repository_id))
}

fn nul_fields(output: &[u8]) -> Vec<&[u8]> {
    output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect()
}

#[cfg(unix)]
fn path_from_git(_repository_id: &str, value: &[u8]) -> Result<PathBuf, ProjectError> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(OsString::from_vec(value.to_vec())))
}

#[cfg(not(unix))]
fn path_from_git(repository_id: &str, value: &[u8]) -> Result<PathBuf, ProjectError> {
    String::from_utf8(value.to_vec())
        .map(PathBuf::from)
        .map_err(|_| git_invalid_output(repository_id))
}

fn validate_relative_git_path(repository_id: &str, path: &Path) -> Result<(), ProjectError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(git_invalid_output(repository_id))
    } else {
        Ok(())
    }
}

fn git_invalid_output(repository_id: &str) -> ProjectError {
    ProjectError::new(
        "task_change_set_git_invalid_output",
        format!("Git returned invalid change data for repository '{repository_id}'"),
    )
}

fn git_checkout_mismatch(repository_id: &str) -> ProjectError {
    ProjectError::new(
        "task_change_set_checkout_mismatch",
        format!("repository '{repository_id}' is not at its owned task checkout"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_parser_preserves_unmerged_and_untracked_states() {
        let mut files = BTreeMap::new();
        parse_worktree_status("repo", b"UU conflict.txt\0?? odd\tname\n.txt\0", &mut files)
            .unwrap();

        assert_eq!(
            files[Path::new("conflict.txt")].kind,
            ChangedFileKind::Unmerged
        );
        assert!(files[Path::new("conflict.txt")].staged);
        assert!(files[Path::new("conflict.txt")].worktree);
        assert_eq!(
            files[Path::new("odd\tname\n.txt")].kind,
            ChangedFileKind::Untracked
        );
    }

    #[test]
    fn parser_rejects_paths_outside_the_checkout() {
        let mut files = BTreeMap::new();
        let error = parse_worktree_status("repo", b"?? ../escape\0", &mut files).unwrap_err();
        assert_eq!(error.code, "task_change_set_git_invalid_output");
    }
}
