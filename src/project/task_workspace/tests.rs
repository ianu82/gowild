use std::collections::BTreeMap;
use std::path::PathBuf;

use super::*;
use crate::project::manifest::{LoadedProject, ResolvedRepo};
use crate::project::model::{ProjectManifest, ProjectRepo};

fn loaded_project() -> LoadedProject {
    LoadedProject {
        manifest_path: PathBuf::from("/projects/example/gowild-project.toml"),
        root: PathBuf::from("/projects/example"),
        digest: "a".repeat(64),
        manifest: ProjectManifest {
            version: 1,
            id: "example-project".into(),
            name: "Example project".into(),
            repositories: vec![
                ProjectRepo {
                    id: "api".into(),
                    path: "api".into(),
                    base: Some("main".into()),
                    depends_on: vec![],
                },
                ProjectRepo {
                    id: "web".into(),
                    path: "web".into(),
                    base: Some("main".into()),
                    depends_on: vec!["api".into()],
                },
            ],
            setup: vec![],
            tests: vec![],
            services: vec![
                crate::project::model::ProjectService {
                    id: "api-service".into(),
                    repository: Some("api".into()),
                    cwd: None,
                    argv: vec!["run-api".into()],
                    environment: BTreeMap::new(),
                    isolation: crate::project::model::RuntimeIsolationSpec {
                        ports: vec!["http".into()],
                        compose: true,
                        ..crate::project::model::RuntimeIsolationSpec::default()
                    },
                },
                crate::project::model::ProjectService {
                    id: "web-service".into(),
                    repository: Some("web".into()),
                    cwd: None,
                    argv: vec!["run-web".into()],
                    environment: BTreeMap::new(),
                    isolation: crate::project::model::RuntimeIsolationSpec {
                        ports: vec!["http".into()],
                        ..crate::project::model::RuntimeIsolationSpec::default()
                    },
                },
            ],
        },
        repositories: vec![
            ResolvedRepo {
                id: "api".into(),
                path: PathBuf::from("/projects/example/api"),
                configured_base: Some("main".into()),
                base_commit: "1".repeat(40),
                head_commit: "1".repeat(40),
                depends_on: vec![],
            },
            ResolvedRepo {
                id: "web".into(),
                path: PathBuf::from("/projects/example/web"),
                configured_base: Some("main".into()),
                base_commit: "2".repeat(40),
                head_commit: "2".repeat(40),
                depends_on: vec!["api".into()],
            },
        ],
    }
}

fn route() -> TaskRoute {
    TaskRoute {
        gateway_id: "mindshub".into(),
        protocol: TaskProtocol::OpenAiResponses,
        model: "openai/gpt-5.6-codex".into(),
    }
}

fn workspace() -> TaskWorkspace {
    TaskWorkspace::new(
        &loaded_project(),
        "task-42",
        "Update the API and its web client",
        TaskAgent::Codex,
        route(),
        PathBuf::from("/state/tasks"),
    )
    .unwrap()
}

#[test]
fn new_workspace_models_every_repo_and_unique_runtime_namespace() {
    let workspace = workspace();

    assert_eq!(workspace.phase, TaskWorkspacePhase::Planned);
    assert_eq!(workspace.repositories.len(), 2);
    assert!(workspace
        .repositories
        .values()
        .all(|repository| repository.worktree.is_none()));
    assert!(workspace.runtime.namespace.starts_with("gw-task-42-"));
    assert_eq!(
        workspace.runtime.environment["GOWILD_TASK_ROOT"],
        workspace.root.display().to_string()
    );
    assert!(!workspace
        .runtime
        .environment
        .keys()
        .any(|key| key.contains("TOKEN") || key.contains("KEY")));

    let second = TaskWorkspace::new(
        &loaded_project(),
        "task-43",
        "Second task",
        TaskAgent::Claude,
        TaskRoute {
            protocol: TaskProtocol::AnthropicMessages,
            ..route()
        },
        PathBuf::from("/state/tasks"),
    )
    .unwrap();
    assert_ne!(workspace.runtime.namespace, second.runtime.namespace);
}

#[test]
fn lifecycle_rejects_skips_and_allows_recovery_and_cleanup() {
    let mut workspace = workspace();
    assert!(workspace
        .transition_phase(TaskWorkspacePhase::Running)
        .is_err());
    workspace
        .transition_phase(TaskWorkspacePhase::Provisioning)
        .unwrap();
    workspace
        .transition_phase(TaskWorkspacePhase::NeedsAttention)
        .unwrap();
    workspace
        .transition_phase(TaskWorkspacePhase::Provisioning)
        .unwrap();
    workspace
        .transition_phase(TaskWorkspacePhase::Ready)
        .unwrap();
    workspace
        .transition_phase(TaskWorkspacePhase::Cleaning)
        .unwrap();
    workspace
        .transition_phase(TaskWorkspacePhase::Cleaned)
        .unwrap();
    assert!(workspace
        .transition_phase(TaskWorkspacePhase::Running)
        .is_err());
}

#[test]
fn planned_worktree_is_applied_then_only_owned_resource_can_be_released() {
    let mut workspace = workspace();
    let checkout_path = workspace.repository_checkout_path("api");
    let resource = OwnedResource::RepositoryWorktree {
        repository_id: "api".into(),
        source_path: PathBuf::from("/projects/example/api"),
        checkout_path: checkout_path.clone(),
        base_commit: "1".repeat(40),
    };
    assert!(workspace
        .plan_transition(TaskTransitionOperation::Release, resource.clone())
        .is_err());

    let acquire = workspace
        .plan_transition(TaskTransitionOperation::Acquire, resource.clone())
        .unwrap();
    assert!(workspace.repositories["api"].worktree.is_none());
    workspace
        .finish_transition(acquire, TaskTransitionState::Applied, None)
        .unwrap();
    assert_eq!(
        workspace.repositories["api"]
            .worktree
            .as_ref()
            .unwrap()
            .checkout_path,
        checkout_path
    );
    assert!(workspace.resource_is_owned(&resource));

    let release = workspace
        .plan_transition(TaskTransitionOperation::Release, resource.clone())
        .unwrap();
    workspace
        .finish_transition(release, TaskTransitionState::Applied, None)
        .unwrap();
    assert!(workspace.repositories["api"].worktree.is_none());
    assert!(!workspace.resource_is_owned(&resource));
}

#[test]
fn branch_activation_requires_owned_checkout_and_task_branch() {
    let mut workspace = workspace();
    let branch = OwnedResource::RepositoryBranch {
        repository_id: "api".into(),
        checkout_path: workspace.repository_checkout_path("api"),
        branch: workspace.branch_name("api"),
        base_commit: "1".repeat(40),
    };
    assert!(workspace
        .plan_transition(TaskTransitionOperation::Acquire, branch.clone())
        .is_err());

    let mut escaped = branch.clone();
    if let OwnedResource::RepositoryBranch { checkout_path, .. } = &mut escaped {
        *checkout_path = PathBuf::from("/tmp/escaped");
    }
    assert!(workspace
        .plan_transition(TaskTransitionOperation::Acquire, escaped)
        .is_err());

    let worktree = OwnedResource::RepositoryWorktree {
        repository_id: "api".into(),
        source_path: PathBuf::from("/projects/example/api"),
        checkout_path: workspace.repository_checkout_path("api"),
        base_commit: "1".repeat(40),
    };
    let worktree_acquire = workspace
        .plan_transition(TaskTransitionOperation::Acquire, worktree.clone())
        .unwrap();
    workspace
        .finish_transition(worktree_acquire, TaskTransitionState::Applied, None)
        .unwrap();
    let branch_acquire = workspace
        .plan_transition(TaskTransitionOperation::Acquire, branch.clone())
        .unwrap();
    workspace
        .finish_transition(branch_acquire, TaskTransitionState::Applied, None)
        .unwrap();
    assert_eq!(
        workspace.repositories["api"]
            .worktree
            .as_ref()
            .and_then(|worktree| worktree.branch.as_deref()),
        Some("gowild/task-42/api")
    );
    assert!(workspace
        .plan_transition(TaskTransitionOperation::Release, worktree.clone())
        .is_err());

    let branch_release = workspace
        .plan_transition(TaskTransitionOperation::Release, branch)
        .unwrap();
    workspace
        .finish_transition(branch_release, TaskTransitionState::Applied, None)
        .unwrap();
    let worktree_release = workspace
        .plan_transition(TaskTransitionOperation::Release, worktree)
        .unwrap();
    workspace
        .finish_transition(worktree_release, TaskTransitionState::Applied, None)
        .unwrap();
    workspace.validate_integrity().unwrap();
}

#[test]
fn journal_requires_stable_codes_and_terminal_entries_cannot_change() {
    let mut workspace = workspace();
    let sequence = workspace
        .plan_transition(
            TaskTransitionOperation::Acquire,
            OwnedResource::RuntimeDirectory {
                path: workspace.runtime.root.clone(),
            },
        )
        .unwrap();
    assert!(workspace
        .finish_transition(sequence, TaskTransitionState::Failed, None)
        .is_err());
    workspace
        .finish_transition(
            sequence,
            TaskTransitionState::Failed,
            Some("directory_create_failed"),
        )
        .unwrap();
    assert!(workspace
        .finish_transition(sequence, TaskTransitionState::Applied, None)
        .is_err());
    assert_eq!(
        workspace.journal[0].failure_code.as_deref(),
        Some("directory_create_failed")
    );
}

#[test]
fn state_validation_rejects_tampering_and_unknown_fields() {
    let project = loaded_project();
    let workspace = workspace();
    let mut value = serde_json::to_value(&workspace).unwrap();
    value["root"] = serde_json::json!("/state/tasks/task-42/../escape");
    let tampered: TaskWorkspace = serde_json::from_value(value).unwrap();
    assert!(tampered.validate(&project).is_err());

    let mut value = serde_json::to_value(&workspace).unwrap();
    value["credential"] = serde_json::json!("never-allowed");
    assert!(serde_json::from_value::<TaskWorkspace>(value).is_err());
}

#[test]
fn stale_manifest_blocks_execution_but_preserves_safe_recovery_validation() {
    let workspace = workspace();
    let mut changed_project = loaded_project();
    changed_project.digest = "b".repeat(64);

    workspace.validate_integrity().unwrap();
    assert!(workspace.require_current_project(&changed_project).is_err());
}

#[test]
fn route_protocol_must_match_the_selected_cli() {
    let error = TaskWorkspace::new(
        &loaded_project(),
        "wrong-route",
        "Reject the incompatible route",
        TaskAgent::Claude,
        route(),
        PathBuf::from("/state/tasks"),
    )
    .unwrap_err();

    assert_eq!(error.code, "invalid_task_workspace_route");
}

#[test]
fn duplicate_ports_and_unowned_resources_are_rejected() {
    let project = loaded_project();
    let mut invalid_ports = workspace();
    invalid_ports.runtime.ports = BTreeMap::from([
        ("api-service.http".into(), 43123),
        ("web-service.http".into(), 43123),
    ]);
    assert!(invalid_ports.validate(&project).is_err());

    let mut outside_boundary = workspace();
    assert!(outside_boundary
        .plan_transition(
            TaskTransitionOperation::Acquire,
            OwnedResource::WorkspaceDirectory {
                path: PathBuf::from("/tmp/not-owned"),
            },
        )
        .is_err());
}
