use std::collections::BTreeMap;

use super::provision::TaskWorkspaceProvisioner;
use super::provision_tests::ProjectFixture;
use super::runtime_commands::TaskCommandKind;
use super::*;
use crate::project::model::ProjectCommand;

#[test]
fn command_plan_uses_the_task_checkout_and_non_secret_runtime_environment() {
    let fixture = command_fixture();
    fixture.create_task("command-plan");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let task = provision_task(&fixture, &provisioner, "command-plan");

    let invocation = provisioner
        .prepare_command(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "command-plan",
            TaskCommandKind::Setup,
            "prepare",
        )
        .unwrap();

    let checkout = task.repositories["shared"]
        .worktree
        .as_ref()
        .unwrap()
        .checkout_path
        .canonicalize()
        .unwrap();
    assert_eq!(invocation.cwd, checkout);
    assert_eq!(invocation.argv, ["git", "rev-parse", "--show-toplevel"]);
    assert_eq!(invocation.environment["FEATURE_MODE"], "isolated");
    assert_eq!(
        invocation.environment["COMPOSE_PROJECT_NAME"],
        task.runtime.namespace
    );
    assert_eq!(
        invocation.environment["TMPDIR"],
        task.runtime.temp.display().to_string()
    );
    assert_eq!(
        invocation.environment["XDG_CACHE_HOME"],
        task.runtime.cache.display().to_string()
    );
    assert_eq!(
        invocation.environment["XDG_DATA_HOME"],
        task.runtime.data.display().to_string()
    );
    assert!(!invocation
        .environment
        .keys()
        .any(|key| key.contains("TOKEN") || key.contains("KEY")));
}

#[test]
fn command_executes_directly_without_mutating_persisted_task_state() {
    let fixture = command_fixture();
    fixture.create_task("command-run");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let task = provision_task(&fixture, &provisioner, "command-run");
    let before = fixture.states.load("command-run").unwrap();

    let result = provisioner
        .run_command(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "command-run",
            TaskCommandKind::Setup,
            "prepare",
        )
        .unwrap();

    let checkout = task.repositories["shared"]
        .worktree
        .as_ref()
        .unwrap()
        .checkout_path
        .canonicalize()
        .unwrap();
    assert!(result.success);
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.cwd, checkout);
    assert_eq!(
        std::path::PathBuf::from(result.stdout.trim())
            .canonicalize()
            .unwrap(),
        checkout
    );
    assert!(result.stderr.is_empty());
    assert!(!result.stdout_truncated);
    assert!(!result.stderr_truncated);
    assert_eq!(fixture.states.load("command-run").unwrap(), before);
}

#[test]
fn nonzero_command_exit_is_a_complete_result_not_an_execution_error() {
    let fixture = command_fixture();
    fixture.create_task("command-failure");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    provision_task(&fixture, &provisioner, "command-failure");

    let result = provisioner
        .run_command(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "command-failure",
            TaskCommandKind::Test,
            "missing-ref",
        )
        .unwrap();

    assert!(!result.success);
    assert!(result.exit_code.is_some_and(|code| code != 0));
}

#[test]
fn command_execution_rechecks_manifest_trust_and_command_identity() {
    let fixture = command_fixture();
    fixture.create_task("command-trust");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    provision_task(&fixture, &provisioner, "command-trust");

    let unknown = provisioner
        .run_command(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "command-trust",
            TaskCommandKind::Setup,
            "not-declared",
        )
        .unwrap_err();
    assert_eq!(unknown.code, "unknown_task_command");

    let mut untrusted = fixture.private_state.clone();
    assert!(untrusted.revoke_trust());
    let error = provisioner
        .run_command(
            &fixture.definition,
            &untrusted,
            &fixture.project,
            "command-trust",
            TaskCommandKind::Setup,
            "prepare",
        )
        .unwrap_err();
    assert_eq!(error.code, "project_manifest_untrusted");
}

#[cfg(unix)]
#[test]
fn command_cwd_refuses_a_symlink_escape_from_the_task_checkout() {
    use std::os::unix::fs::symlink;

    let mut fixture = command_fixture();
    fixture.definition.manifest.setup[0].cwd = Some("escape".into());
    fixture.project.manifest.setup[0].cwd = Some("escape".into());
    fixture.create_task("command-escape");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let task = provision_task(&fixture, &provisioner, "command-escape");
    let outside = fixture.root.join("outside-command");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"outside\n").unwrap();
    let checkout = &task.repositories["shared"]
        .worktree
        .as_ref()
        .unwrap()
        .checkout_path;
    symlink(&outside, checkout.join("escape")).unwrap();

    let error = provisioner
        .prepare_command(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "command-escape",
            TaskCommandKind::Setup,
            "prepare",
        )
        .unwrap_err();

    assert_eq!(error.code, "task_command_cwd_escape");
    assert_eq!(
        std::fs::read(outside.join("keep.txt")).unwrap(),
        b"outside\n"
    );
}

fn command_fixture() -> ProjectFixture {
    let mut fixture = ProjectFixture::new(true);
    for manifest in [
        &mut fixture.definition.manifest,
        &mut fixture.project.manifest,
    ] {
        manifest.setup[0] = ProjectCommand {
            id: "prepare".into(),
            repository: Some("shared".into()),
            cwd: None,
            argv: vec!["git".into(), "rev-parse".into(), "--show-toplevel".into()],
            environment: BTreeMap::from([("FEATURE_MODE".into(), "isolated".into())]),
        };
        manifest.tests.push(ProjectCommand {
            id: "missing-ref".into(),
            repository: Some("api".into()),
            cwd: None,
            argv: vec![
                "git".into(),
                "rev-parse".into(),
                "--verify".into(),
                "refs/heads/does-not-exist".into(),
            ],
            environment: BTreeMap::new(),
        });
        manifest.validate().unwrap();
    }
    fixture
        .private_state
        .grant_trust(&fixture.definition, &fixture.definition.digest)
        .unwrap();
    fixture
}

fn provision_task(
    fixture: &ProjectFixture,
    provisioner: &TaskWorkspaceProvisioner<'_>,
    task_id: &str,
) -> TaskWorkspace {
    provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            task_id,
        )
        .unwrap()
}
