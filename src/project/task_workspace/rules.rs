use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

use super::{OwnedResource, TaskRepository, TaskWorkspacePhase};
use crate::project::ProjectError;

pub(super) fn phase_transition_allowed(
    current: TaskWorkspacePhase,
    next: TaskWorkspacePhase,
) -> bool {
    use TaskWorkspacePhase::{
        Cleaned, Cleaning, NeedsAttention, Planned, Provisioning, Ready, Running, Stopped,
    };
    matches!(
        (current, next),
        (Planned, Provisioning | Cleaning)
            | (Provisioning, Ready | Cleaning | NeedsAttention)
            | (Ready, Running | Stopped | Cleaning | NeedsAttention)
            | (Running, Stopped | NeedsAttention)
            | (Stopped, Running | Cleaning | NeedsAttention)
            | (NeedsAttention, Provisioning | Running | Stopped | Cleaning)
            | (Cleaning, Cleaned | NeedsAttention)
    )
}

pub(super) fn resources_conflict(left: &OwnedResource, right: &OwnedResource) -> bool {
    match (left, right) {
        (
            OwnedResource::WorkspaceDirectory { path: left },
            OwnedResource::WorkspaceDirectory { path: right },
        )
        | (
            OwnedResource::RuntimeDirectory { path: left },
            OwnedResource::RuntimeDirectory { path: right },
        ) => left == right,
        (
            OwnedResource::RepositoryWorktree {
                repository_id: left_id,
                checkout_path: left_path,
                ..
            },
            OwnedResource::RepositoryWorktree {
                repository_id: right_id,
                checkout_path: right_path,
                ..
            },
        ) => left_id == right_id || left_path == right_path,
        (
            OwnedResource::RepositoryBranch {
                repository_id: left_id,
                branch: left_branch,
                ..
            },
            OwnedResource::RepositoryBranch {
                repository_id: right_id,
                branch: right_branch,
                ..
            },
        ) => left_id == right_id || left_branch == right_branch,
        (
            OwnedResource::PortReservation {
                name: left_name,
                port: left_port,
            },
            OwnedResource::PortReservation {
                name: right_name,
                port: right_port,
            },
        ) => left_name == right_name || left_port == right_port,
        (OwnedResource::ComposeProject { .. }, OwnedResource::ComposeProject { .. }) => true,
        (
            OwnedResource::ServiceProcess {
                service_id: left, ..
            },
            OwnedResource::ServiceProcess {
                service_id: right, ..
            },
        ) => left == right,
        _ => false,
    }
}

pub(super) fn validate_dependency_graph(
    repositories: &BTreeMap<String, TaskRepository>,
) -> Result<(), ProjectError> {
    let mut resolved = BTreeSet::new();
    loop {
        let previous_len = resolved.len();
        for (repository_id, repository) in repositories {
            if repository
                .depends_on
                .iter()
                .all(|dependency| resolved.contains(dependency.as_str()))
            {
                resolved.insert(repository_id.as_str());
            }
        }
        if resolved.len() == repositories.len() {
            return Ok(());
        }
        if resolved.len() == previous_len {
            return Err(ProjectError::new(
                "task_workspace_dependency_cycle",
                "task workspace repository dependencies contain a cycle",
            ));
        }
    }
}

pub(in crate::project) fn validate_identifier(
    label: &str,
    value: &str,
) -> Result<(), ProjectError> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        || value.starts_with(['-', '.', '_'])
        || value.ends_with(['-', '.', '_'])
        || value.contains("..")
    {
        return Err(ProjectError::new(
            "invalid_task_workspace_identifier",
            format!("{label} is not a safe lowercase identifier"),
        ));
    }
    Ok(())
}

pub(super) fn validate_manifest_identifier(label: &str, value: &str) -> Result<(), ProjectError> {
    let valid = !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.ends_with('-')
        && !value.contains("--");
    if valid {
        Ok(())
    } else {
        Err(ProjectError::new(
            "invalid_task_workspace_identifier",
            format!("{label} is not a safe project-manifest identifier"),
        ))
    }
}

pub(super) fn validate_outcome(outcome: &str) -> Result<(), ProjectError> {
    if outcome.trim().is_empty()
        || outcome.len() > 4_096
        || outcome.chars().any(|character| {
            character == '\0'
                || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        })
    {
        return Err(ProjectError::new(
            "invalid_task_workspace_outcome",
            "task outcome must be non-empty, at most 4096 bytes, and contain no unsafe control characters",
        ));
    }
    Ok(())
}

pub(super) fn validate_absolute_clean_path(label: &str, path: &Path) -> Result<(), ProjectError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(ProjectError::new(
            "invalid_task_workspace_path",
            format!("{label} must be an absolute normalized path"),
        ));
    }
    Ok(())
}

pub(in crate::project) fn validate_digest(
    label: &str,
    digest: &str,
    length: usize,
) -> Result<(), ProjectError> {
    if digest.len() != length
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProjectError::new(
            "invalid_task_workspace_digest",
            format!("{label} is not a lowercase hexadecimal digest"),
        ));
    }
    Ok(())
}

pub(in crate::project) fn validate_git_object_id(value: &str) -> Result<(), ProjectError> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProjectError::new(
            "invalid_task_workspace_git_object",
            "repository commit is not a lowercase hexadecimal Git object id",
        ));
    }
    Ok(())
}

pub(super) fn runtime_namespace(project_id: &str, task_id: &str, manifest_digest: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_id.as_bytes());
    hasher.update([0]);
    hasher.update(task_id.as_bytes());
    hasher.update([0]);
    hasher.update(manifest_digest.as_bytes());
    let digest = hasher.finalize();
    let suffix = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut task_slug = task_id.chars().take(24).collect::<String>();
    task_slug = task_slug.trim_matches(['-', '.', '_']).to_string();
    format!("gw-{task_slug}-{suffix}")
}

pub(super) fn service_instance_id(namespace: &str, service_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update([0]);
    hasher.update(service_id.as_bytes());
    let digest = hasher.finalize();
    let suffix = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("svc-{suffix}")
}
