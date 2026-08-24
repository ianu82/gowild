use super::provision::TaskWorkspaceProvisioner;
use super::provision_tests::ProjectFixture;
use super::runtime_compose::{
    prepare_compose_invocation, SystemTaskComposeRuntime, TaskComposeRuntime,
};
use super::*;
use crate::project::model::{ProjectService, RuntimeIsolationSpec};
use std::collections::BTreeMap;

#[test]
fn compose_plan_pins_the_task_checkout_namespace_and_default_file() {
    let fixture = compose_fixture(vec!["docker", "compose", "up", "--detach"]);
    let task = provision_task(&fixture, "compose-plan");
    let checkout = task.repositories["api"]
        .worktree
        .as_ref()
        .unwrap()
        .checkout_path
        .clone();
    std::fs::write(checkout.join("compose.yaml"), b"services: {}\n").unwrap();

    let invocation =
        prepare_compose_invocation(&task, &fixture.project.manifest.services[0]).unwrap();

    assert_eq!(
        invocation.argv,
        [
            "docker",
            "compose",
            "--file",
            "compose.yaml",
            "up",
            "--detach"
        ]
    );
    assert_eq!(invocation.cwd, checkout.canonicalize().unwrap());
    assert_eq!(
        invocation.control.project_name,
        task.runtime.compose_project
    );
    assert_eq!(
        invocation.environment["COMPOSE_PROJECT_NAME"],
        task.runtime.namespace
    );
    assert_eq!(invocation.environment["FEATURE_MODE"], "isolated");
    assert!(!invocation.control.environment.contains_key("FEATURE_MODE"));
    assert_eq!(
        invocation.control.descriptor_path,
        task.runtime.root.join("control/compose/stack.json")
    );
}

#[test]
fn compose_plan_rejects_missing_and_escaping_files() {
    let fixture = compose_fixture(vec![
        "docker",
        "compose",
        "--file",
        "../shared/README.md",
        "up",
        "-d",
    ]);
    let task = provision_task(&fixture, "compose-escape");
    assert_eq!(
        prepare_compose_invocation(&task, &fixture.project.manifest.services[0])
            .unwrap_err()
            .code,
        "task_compose_command_invalid"
    );

    let missing = compose_fixture(vec!["docker", "compose", "up", "-d"]);
    let task = provision_task(&missing, "compose-missing");
    assert_eq!(
        prepare_compose_invocation(&task, &missing.project.manifest.services[0])
            .unwrap_err()
            .code,
        "task_compose_file_missing"
    );
}

#[cfg(unix)]
#[test]
fn compose_plan_rejects_symlinked_files() {
    use std::os::unix::fs::symlink;

    let fixture = compose_fixture(vec![
        "docker",
        "compose",
        "--file",
        "compose.yaml",
        "up",
        "-d",
    ]);
    let task = provision_task(&fixture, "compose-symlink");
    let checkout = &task.repositories["api"]
        .worktree
        .as_ref()
        .unwrap()
        .checkout_path;
    symlink(checkout.join("README.md"), checkout.join("compose.yaml")).unwrap();

    assert_eq!(
        prepare_compose_invocation(&task, &fixture.project.manifest.services[0])
            .unwrap_err()
            .code,
        "task_compose_command_invalid"
    );
}

#[cfg(unix)]
#[test]
fn compose_runtime_refuses_a_symlinked_control_record() {
    use std::os::unix::fs::symlink;

    let fixture = compose_fixture(vec!["docker", "compose", "--file", "README.md", "up", "-d"]);
    let task = provision_task(&fixture, "compose-control-symlink");
    let invocation =
        prepare_compose_invocation(&task, &fixture.project.manifest.services[0]).unwrap();
    std::fs::create_dir_all(invocation.control.descriptor_path.parent().unwrap()).unwrap();
    symlink(
        task.repositories["api"]
            .worktree
            .as_ref()
            .unwrap()
            .checkout_path
            .join("README.md"),
        &invocation.control.descriptor_path,
    )
    .unwrap();

    assert_eq!(
        SystemTaskComposeRuntime
            .ensure_up(&invocation)
            .unwrap_err()
            .code,
        "task_compose_control_invalid"
    );
}

#[cfg(unix)]
#[test]
fn system_compose_runtime_is_idempotent_bounded_and_recoverable() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = ProjectFixture::new(false);
    let docker = fixture.root.join("docker");
    std::fs::write(
        &docker,
        r#"#!/bin/sh
set -eu
operation=""
for argument in "$@"; do
  case "$argument" in
    up|ps|down) operation="$argument" ;;
  esac
done
state="$GOWILD_RUNTIME_ROOT/compose-running"
count="$GOWILD_RUNTIME_ROOT/compose-up-count"
case "$operation" in
  up)
    value=0
    if [ -f "$count" ]; then value=$(cat "$count"); fi
    value=$((value + 1))
    printf '%s\n' "$value" > "$count"
    printf '%s\n%s\n' "$PWD" "$COMPOSE_PROJECT_NAME" > "$GOWILD_RUNTIME_ROOT/compose-observed"
    : > "$state"
    ;;
  ps)
    if [ -f "$state" ]; then printf 'container-id\n'; fi
    ;;
  down)
    rm -f "$state"
    ;;
  *) exit 9 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700)).unwrap();
    install_compose_service(
        &mut fixture,
        vec![
            docker.display().to_string(),
            "compose".into(),
            "--file".into(),
            "README.md".into(),
            "up".into(),
            "--detach".into(),
        ],
    );
    let task = provision_task(&fixture, "compose-system");
    let invocation =
        prepare_compose_invocation(&task, &fixture.project.manifest.services[0]).unwrap();
    let runtime = SystemTaskComposeRuntime;

    runtime.ensure_up(&invocation).unwrap();
    runtime.ensure_up(&invocation).unwrap();
    runtime.verify(&invocation.control).unwrap();

    assert_eq!(
        std::fs::read_to_string(task.runtime.root.join("compose-up-count"))
            .unwrap()
            .trim(),
        "1"
    );
    let observed = std::fs::read_to_string(task.runtime.root.join("compose-observed")).unwrap();
    assert_eq!(
        observed.lines().collect::<Vec<_>>(),
        [
            invocation.cwd.to_str().unwrap(),
            task.runtime.namespace.as_str()
        ]
    );
    let descriptor = std::fs::read_to_string(&invocation.control.descriptor_path).unwrap();
    assert!(!descriptor.contains("FEATURE_MODE") && !descriptor.contains("isolated"));
    assert_eq!(
        std::fs::metadata(&invocation.control.descriptor_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    runtime.down(&invocation.control).unwrap();
    assert_eq!(
        runtime.verify(&invocation.control).unwrap_err().code,
        "task_compose_not_running"
    );
}

fn compose_fixture(argv: Vec<&str>) -> ProjectFixture {
    let mut fixture = ProjectFixture::new(false);
    install_compose_service(&mut fixture, argv.into_iter().map(str::to_string).collect());
    fixture
}

fn install_compose_service(fixture: &mut ProjectFixture, argv: Vec<String>) {
    let service = ProjectService {
        id: "app-stack".into(),
        repository: Some("api".into()),
        cwd: None,
        argv,
        environment: BTreeMap::from([("FEATURE_MODE".into(), "isolated".into())]),
        isolation: RuntimeIsolationSpec {
            compose: true,
            ..RuntimeIsolationSpec::default()
        },
    };
    fixture.definition.manifest.services = vec![service.clone()];
    fixture.project.manifest.services = vec![service];
    fixture.definition.manifest.validate().unwrap();
    fixture.project.manifest.validate().unwrap();
    let digest = fixture.definition.digest.clone();
    fixture
        .private_state
        .grant_trust(&fixture.definition, &digest)
        .unwrap();
}

fn provision_task(fixture: &ProjectFixture, task_id: &str) -> TaskWorkspace {
    fixture.create_task(task_id);
    TaskWorkspaceProvisioner::new(&fixture.states)
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            task_id,
        )
        .unwrap()
}
