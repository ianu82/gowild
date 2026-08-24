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

    let mut derived_state_tamper = workspace;
    let tampered_checkout_path = derived_state_tamper.repository_checkout_path("api");
    derived_state_tamper
        .repositories
        .get_mut("api")
        .unwrap()
        .worktree = Some(TaskWorktree {
        checkout_path: tampered_checkout_path,
        head_commit: "1".repeat(40),
        branch: None,
    });
    assert!(derived_state_tamper.validate_integrity().is_err());
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
fn duplicate_runtime_ports_are_rejected() {
    let project = loaded_project();
    let mut invalid_ports = workspace();
    invalid_ports.runtime.ports = BTreeMap::from([
        ("api-service.http".into(), 43123),
        ("web-service.http".into(), 43123),
    ]);
    assert!(invalid_ports.validate(&project).is_err());
}
