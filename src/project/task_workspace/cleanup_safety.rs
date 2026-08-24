use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use super::provision::{git_stdout, task_root_marker, task_worktree_entry, verify_owned_task_root};
use super::runtime_layout::{runtime_directories, validate_releasable_runtime_directory};
use super::{OwnedResource, TaskTransitionOperation, TaskTransitionState, TaskWorkspace};
use crate::project::ProjectError;

pub(super) fn preflight_cleanup(task: &TaskWorkspace) -> Result<(), ProjectError> {
    let root_resource = OwnedResource::WorkspaceDirectory {
        path: task.root.clone(),
    };
    let root_release_may_be_partial = release_may_be_partial(task, &root_resource);
    if task.resource_is_owned(&root_resource) {
        validate_releasable_root(task, root_release_may_be_partial)?;
    } else {
        match fs::symlink_metadata(&task.root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(cleanup_io("task workspace root metadata", &error)),
            Ok(_) => {
                return Err(cleanup_conflict(
                    "task workspace root exists without durable ownership".into(),
                ));
            }
        }
    }

    for path in runtime_directories(task) {
        let resource = OwnedResource::RuntimeDirectory { path: path.clone() };
        if task.resource_is_owned(&resource) {
            validate_releasable_runtime_directory(
                task,
                &path,
                release_may_be_partial(task, &resource),
            )?;
        } else {
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(cleanup_io("task runtime metadata", &error)),
                Ok(_) => {
                    return Err(cleanup_conflict(
                        "task runtime directory exists without durable ownership".into(),
                    ));
                }
            }
        }
    }

    for repository_id in reverse_repository_order(task)? {
        let worktree = worktree_resource(task, &repository_id);
        let worktree_release_may_be_partial = release_may_be_partial(task, &worktree);
        preflight_worktree(task, &repository_id, worktree_release_may_be_partial)?;
    }
    Ok(())
}

fn preflight_worktree(
    task: &TaskWorkspace,
    repository_id: &str,
    worktree_release_may_be_partial: bool,
) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let Some(worktree) = &repository.worktree else {
        return Ok(());
    };
    let checkout = task.repository_checkout_path(repository_id);
    let Some(entry) = task_worktree_entry(&repository.source_path, &checkout)? else {
        if worktree_release_may_be_partial && !checkout.exists() {
            return Ok(());
        }
        return Err(cleanup_conflict(format!(
            "repository '{repository_id}' task worktree is missing or unowned"
        )));
    };
    if entry.is_bare || entry.is_prunable {
        return Err(cleanup_conflict(format!(
            "repository '{repository_id}' task worktree ownership is ambiguous"
        )));
    }
    let dirty = crate::worktree::checkout_has_dirty_files(&checkout).map_err(|_| {
        cleanup_git_failed(format!(
            "Git could not inspect repository '{repository_id}' task changes"
        ))
    })?;
    if dirty {
        return Err(ProjectError::new(
            "task_workspace_cleanup_dirty",
            format!("repository '{repository_id}' has dirty task changes"),
        ));
    }

    if let Some(branch) = &worktree.branch {
        preflight_branch(task, repository_id, branch, &entry)
    } else {
        let head = git_stdout(repository_id, &checkout, &["rev-parse", "--verify", "HEAD"])?;
        if head == repository.base_commit || branch_release_was_applied(task, repository_id) {
            Ok(())
        } else {
            Err(ProjectError::new(
                "task_workspace_cleanup_unpushed",
                format!("repository '{repository_id}' has an unowned detached commit"),
            ))
        }
    }
}

fn preflight_branch(
    task: &TaskWorkspace,
    repository_id: &str,
    branch: &str,
    entry: &crate::worktree::ExistingWorktree,
) -> Result<(), ProjectError> {
    let repository = &task.repositories[repository_id];
    let resource = branch_resource(task, repository_id).ok_or_else(|| {
        cleanup_conflict(format!(
            "repository '{repository_id}' branch ownership is missing"
        ))
    })?;
    let release_may_be_partial = release_may_be_partial(task, &resource);
    let release_applied = branch_release_was_applied(task, repository_id);
    if entry.branch.as_deref() != Some(branch)
        && !(entry.is_detached && (release_may_be_partial || release_applied))
    {
        return Err(cleanup_conflict(format!(
            "repository '{repository_id}' task branch is checked out elsewhere"
        )));
    }
    let Some(branch_head) = branch_commit(repository_id, &repository.source_path, branch)? else {
        return if release_may_be_partial || release_applied {
            Ok(())
        } else {
            Err(cleanup_conflict(format!(
                "repository '{repository_id}' task branch is missing"
            )))
        };
    };
    let checkout_head = git_stdout(
        repository_id,
        &task.repository_checkout_path(repository_id),
        &["rev-parse", "--verify", "HEAD"],
    )?;
    if checkout_head != branch_head {
        return Err(cleanup_conflict(format!(
            "repository '{repository_id}' task branch and checkout disagree"
        )));
    }
    if branch_head == repository.base_commit {
        return Ok(());
    }
    let reference = format!("refs/heads/{branch}");
    let upstream = git_optional_stdout(
        repository_id,
        &repository.source_path,
        &["for-each-ref", "--format=%(upstream)", &reference],
    )?
    .unwrap_or_default();
    if upstream.is_empty() {
        return Err(ProjectError::new(
            "task_workspace_cleanup_unpushed",
            format!("repository '{repository_id}' task branch has unpushed commits"),
        ));
    }
    let range = format!("{upstream}..{reference}");
    let ahead = git_stdout(
        repository_id,
        &repository.source_path,
        &["rev-list", "--count", &range],
    )?;
    if ahead == "0" {
        Ok(())
    } else {
        Err(ProjectError::new(
            "task_workspace_cleanup_unpushed",
            format!("repository '{repository_id}' task branch has unpushed commits"),
        ))
    }
}

pub(super) fn validate_releasable_root(
    task: &TaskWorkspace,
    allow_partial: bool,
) -> Result<(), ProjectError> {
    match fs::symlink_metadata(&task.root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return if allow_partial {
                Ok(())
            } else {
                Err(cleanup_conflict("task workspace root is missing".into()))
            };
        }
        Err(error) => return Err(cleanup_io("task workspace root metadata", &error)),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(cleanup_conflict(
                "task workspace root is a symlink or non-directory".into(),
            ));
        }
        Ok(_) => {}
    }
    if !allow_partial {
        verify_owned_task_root(task)?;
    }
    let marker_name = task_root_marker(task)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| cleanup_conflict("task ownership marker is invalid".into()))?
        .to_string();
    for entry in fs::read_dir(&task.root).map_err(|error| cleanup_io("task root read", &error))? {
        let entry = entry.map_err(|error| cleanup_io("task root entry", &error))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| cleanup_conflict("task root contains an invalid entry".into()))?;
        if name != "repositories" && name != "runtime" && name != marker_name {
            return Err(cleanup_conflict(format!(
                "task workspace root contains unowned entry '{name}'"
            )));
        }
    }
    let repositories = task.root.join("repositories");
    if repositories.exists() {
        for entry in fs::read_dir(&repositories)
            .map_err(|error| cleanup_io("task repository directory read", &error))?
        {
            let entry = entry.map_err(|error| cleanup_io("task repository entry", &error))?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                cleanup_conflict("task repository directory contains an invalid entry".into())
            })?;
            if !task.repositories.contains_key(name) {
                return Err(cleanup_conflict(format!(
                    "task repository directory contains unowned entry '{name}'"
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn reverse_repository_order(task: &TaskWorkspace) -> Result<Vec<String>, ProjectError> {
    let mut resolved = BTreeSet::new();
    let mut order = Vec::with_capacity(task.repositories.len());
    while order.len() < task.repositories.len() {
        let previous = order.len();
        for (repository_id, repository) in &task.repositories {
            if !resolved.contains(repository_id)
                && repository
                    .depends_on
                    .iter()
                    .all(|dependency| resolved.contains(dependency))
            {
                resolved.insert(repository_id.clone());
                order.push(repository_id.clone());
            }
        }
        if order.len() == previous {
            return Err(ProjectError::new(
                "task_workspace_repository_dependency_cycle",
                "task repository dependency order could not be resolved",
            ));
        }
    }
    order.reverse();
    Ok(order)
}

pub(super) fn worktree_resource(task: &TaskWorkspace, repository_id: &str) -> OwnedResource {
    let repository = &task.repositories[repository_id];
    OwnedResource::RepositoryWorktree {
        repository_id: repository_id.to_string(),
        source_path: repository.source_path.clone(),
        checkout_path: task.repository_checkout_path(repository_id),
        base_commit: repository.base_commit.clone(),
    }
}

pub(super) fn branch_resource(task: &TaskWorkspace, repository_id: &str) -> Option<OwnedResource> {
    let repository = &task.repositories[repository_id];
    repository
        .worktree
        .as_ref()?
        .branch
        .as_ref()
        .map(|branch| OwnedResource::RepositoryBranch {
            repository_id: repository_id.to_string(),
            checkout_path: task.repository_checkout_path(repository_id),
            branch: branch.clone(),
            base_commit: repository.base_commit.clone(),
        })
}

fn release_may_be_partial(task: &TaskWorkspace, resource: &OwnedResource) -> bool {
    task.journal.iter().any(|transition| {
        transition.operation == TaskTransitionOperation::Release
            && matches!(
                transition.state,
                TaskTransitionState::Planned | TaskTransitionState::Failed
            )
            && &transition.resource == resource
    })
}

fn branch_release_was_applied(task: &TaskWorkspace, repository_id: &str) -> bool {
    task.journal.iter().any(|transition| {
        transition.operation == TaskTransitionOperation::Release
            && transition.state == TaskTransitionState::Applied
            && matches!(
                &transition.resource,
                OwnedResource::RepositoryBranch {
                    repository_id: released,
                    ..
                } if released == repository_id
            )
    })
}

pub(super) fn branch_commit(
    repository_id: &str,
    source_path: &Path,
    branch: &str,
) -> Result<Option<String>, ProjectError> {
    let exists = crate::worktree::local_branch_exists(source_path, branch).map_err(|_| {
        cleanup_git_failed(format!(
            "Git could not inspect repository '{repository_id}' task branch"
        ))
    })?;
    if !exists {
        return Ok(None);
    }
    let reference = format!("refs/heads/{branch}");
    git_stdout(
        repository_id,
        source_path,
        &["rev-parse", "--verify", &reference],
    )
    .map(Some)
}

fn git_optional_stdout(
    repository_id: &str,
    cwd: &Path,
    args: &[&str],
) -> Result<Option<String>, ProjectError> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|_| {
            cleanup_git_failed(format!(
                "Git could not inspect repository '{repository_id}' cleanup state"
            ))
        })?;
    if !output.status.success() {
        return Err(cleanup_git_failed(format!(
            "Git could not inspect repository '{repository_id}' cleanup state"
        )));
    }
    Ok(String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty()))
}

pub(super) fn cleanup_conflict(message: String) -> ProjectError {
    ProjectError::new("task_workspace_cleanup_conflict", message)
}

pub(super) fn cleanup_git_failed(message: String) -> ProjectError {
    ProjectError::new("task_workspace_cleanup_git_failed", message)
}

pub(super) fn cleanup_io(operation: &'static str, error: &io::Error) -> ProjectError {
    ProjectError::new(
        "task_workspace_cleanup_io",
        format!("{operation} failed ({:?})", error.kind()),
    )
}
