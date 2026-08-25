use std::collections::BTreeMap;

use super::*;
use crate::project::task_workspace::provision::TaskWorkspaceProvisioner;
use crate::project::task_workspace::provision_tests::ProjectFixture;

#[test]
fn plan_contains_only_affected_repositories_in_dependency_order() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("review");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "review",
        )
        .unwrap();
    for repository_id in ["api", "web"] {
        provisioner
            .activate_repository(
                &fixture.definition,
                &fixture.private_state,
                &fixture.project,
                "review",
                repository_id,
            )
            .unwrap();
        std::fs::write(
            task.repositories[repository_id]
                .worktree
                .as_ref()
                .unwrap()
                .checkout_path
                .join(format!("{repository_id}.txt")),
            format!("{repository_id}\n"),
        )
        .unwrap();
    }
    let mut change_set = fixture
        .states
        .inspect_change_set(&fixture.project, "review")
        .unwrap();
    let bases = BTreeMap::from([
        ("api".into(), "main".into()),
        ("web".into(), "release/2026.08".into()),
    ]);

    let first_plan = change_set.plan_draft_pull_requests(&bases).unwrap().clone();
    let second_plan = change_set.plan_draft_pull_requests(&bases).unwrap().clone();

    assert_eq!(first_plan, second_plan);
    assert_eq!(first_plan.len(), 2);
    assert_eq!(first_plan["api"].position, 1);
    assert!(first_plan["api"].depends_on.is_empty());
    assert_eq!(first_plan["web"].position, 2);
    assert_eq!(first_plan["web"].depends_on, ["api"]);
    assert_eq!(first_plan["api"].head_branch, "gowild/review/api");
    assert_eq!(first_plan["web"].base_branch, "release/2026.08");
    assert!(first_plan["web"].body.contains("Merge position: 2"));
    assert!(!first_plan.contains_key("shared"));
    assert!(!change_set.merge_is_approved());
}

#[test]
fn plan_requires_inspection_changes_task_branches_and_explicit_safe_bases() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("gates");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "gates",
        )
        .unwrap();
    let mut pending = ChangeSet::for_task(&task).unwrap();
    let error = pending
        .plan_draft_pull_requests(&BTreeMap::new())
        .unwrap_err();
    assert_eq!(error.code, "task_change_set_not_inspected");

    let mut clean = fixture
        .states
        .inspect_change_set(&fixture.project, "gates")
        .unwrap();
    let error = clean
        .plan_draft_pull_requests(&BTreeMap::new())
        .unwrap_err();
    assert_eq!(error.code, "task_change_set_has_no_changes");

    let api = &task.repositories["api"]
        .worktree
        .as_ref()
        .unwrap()
        .checkout_path;
    std::fs::write(api.join("change.txt"), b"change\n").unwrap();
    let mut detached = fixture
        .states
        .inspect_change_set(&fixture.project, "gates")
        .unwrap();
    let error = detached
        .plan_draft_pull_requests(&BTreeMap::from([("api".into(), "main".into())]))
        .unwrap_err();
    assert_eq!(error.code, "task_change_set_branch_required");

    provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "gates",
            "api",
        )
        .unwrap();
    let mut missing_base = fixture
        .states
        .inspect_change_set(&fixture.project, "gates")
        .unwrap();
    let error = missing_base
        .plan_draft_pull_requests(&BTreeMap::new())
        .unwrap_err();
    assert_eq!(error.code, "task_change_set_base_branch_required");
    let error = missing_base
        .plan_draft_pull_requests(&BTreeMap::from([("api".into(), "bad..base".into())]))
        .unwrap_err();
    assert_eq!(error.code, "task_change_set_invalid_branch");
}
