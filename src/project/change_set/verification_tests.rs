use super::*;
use crate::project::model::ProjectCommand;
use crate::project::task_workspace::provision::TaskWorkspaceProvisioner;
use crate::project::task_workspace::provision_tests::ProjectFixture;
use crate::project::task_workspace::TaskWorkspacePhase;

#[test]
fn verification_collects_every_result_without_retaining_command_output() {
    let fixture = verification_fixture();
    fixture.create_task("verified");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "verified",
        )
        .unwrap();

    let change_set = provisioner
        .verify_change_set(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "verified",
        )
        .unwrap();

    assert_eq!(change_set.checks.len(), 4);
    assert_eq!(change_set.checks["pass"].status, CheckStatus::Passed);
    assert_eq!(change_set.checks["pass"].exit_code, Some(0));
    assert_eq!(change_set.checks["fail"].status, CheckStatus::Failed);
    assert!(change_set.checks["fail"]
        .exit_code
        .is_some_and(|code| code != 0));
    assert_eq!(
        change_set.checks["missing-program"].failure_code.as_deref(),
        Some("task_command_spawn_failed")
    );
    assert_eq!(
        change_set.checks["credential-noise"]
            .repository_id
            .as_deref(),
        Some("shared")
    );
    assert!(change_set
        .checks
        .values()
        .all(|check| check.duration_ms.is_some()));
    assert!(change_set.affected_repository_ids().is_empty());

    let serialized = serde_json::to_string(&change_set).unwrap();
    assert!(!serialized.contains("tests@gowild.invalid"));
    assert!(!serialized.contains("GoWild Tests"));
}

#[test]
fn verification_requires_current_trust_and_a_non_running_task() {
    let fixture = verification_fixture();
    fixture.create_task("gated");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let mut task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "gated",
        )
        .unwrap();

    let mut revoked = fixture.private_state.clone();
    assert!(revoked.revoke_trust());
    let error = provisioner
        .verify_change_set(&fixture.definition, &revoked, &fixture.project, "gated")
        .unwrap_err();
    assert_eq!(error.code, "project_manifest_untrusted");

    let revision = task.revision;
    task.transition_phase(TaskWorkspacePhase::Running).unwrap();
    fixture.states.save(&task, revision).unwrap();
    let error = provisioner
        .verify_change_set(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "gated",
        )
        .unwrap_err();
    assert_eq!(error.code, "task_change_set_not_verifiable");
}

fn verification_fixture() -> ProjectFixture {
    let mut fixture = ProjectFixture::new(true);
    let tests = vec![
        ProjectCommand {
            id: "pass".into(),
            repository: Some("shared".into()),
            cwd: None,
            argv: vec![
                "git".into(),
                "rev-parse".into(),
                "--verify".into(),
                "HEAD".into(),
            ],
            environment: Default::default(),
        },
        ProjectCommand {
            id: "fail".into(),
            repository: Some("api".into()),
            cwd: None,
            argv: vec![
                "git".into(),
                "rev-parse".into(),
                "--verify".into(),
                "refs/heads/does-not-exist".into(),
            ],
            environment: Default::default(),
        },
        ProjectCommand {
            id: "missing-program".into(),
            repository: Some("web".into()),
            cwd: None,
            argv: vec!["gowild-test-program-that-does-not-exist".into()],
            environment: Default::default(),
        },
        ProjectCommand {
            id: "credential-noise".into(),
            repository: Some("shared".into()),
            cwd: None,
            argv: vec![
                "git".into(),
                "config".into(),
                "--get".into(),
                "user.email".into(),
            ],
            environment: Default::default(),
        },
    ];
    for manifest in [
        &mut fixture.definition.manifest,
        &mut fixture.project.manifest,
    ] {
        manifest.tests.clone_from(&tests);
        manifest.validate().unwrap();
    }
    fixture
        .private_state
        .grant_trust(&fixture.definition, &fixture.definition.digest)
        .unwrap();
    fixture
}
