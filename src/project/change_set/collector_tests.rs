use std::path::Path;
use std::process::Command;

use super::*;
use crate::project::task_workspace::provision::TaskWorkspaceProvisioner;
use crate::project::task_workspace::provision_tests::ProjectFixture;

#[test]
fn inspection_combines_clean_committed_staged_unstaged_and_untracked_repositories() {
    let fixture = ProjectFixture::new(false);
    fixture.create_task("change-set");
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    let task = provisioner
        .provision(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "change-set",
        )
        .unwrap();
    for repository_id in ["api", "web"] {
        provisioner
            .activate_repository(
                &fixture.definition,
                &fixture.private_state,
                &fixture.project,
                "change-set",
                repository_id,
            )
            .unwrap();
    }

    let api = checkout(&task, "api");
    let mut readme = std::fs::read(api.join("README.md")).unwrap();
    readme.extend_from_slice(b"working tree\n");
    std::fs::write(api.join("README.md"), readme).unwrap();
    std::fs::write(api.join("staged.txt"), b"staged\n").unwrap();
    run_git(api, &["add", "staged.txt"]);
    let odd_path = Path::new("odd\tname\n.txt");
    std::fs::write(api.join(odd_path), b"untracked\n").unwrap();

    let web = checkout(&task, "web");
    run_git(web, &["mv", "README.md", "WEB.md"]);
    run_git(web, &["commit", "-m", "rename web readme"]);

    let change_set = fixture
        .states
        .inspect_change_set(&fixture.project, "change-set")
        .unwrap();

    assert_eq!(change_set.dependency_order, ["shared", "api", "web"]);
    assert_eq!(change_set.affected_repository_ids(), ["api", "web"]);
    assert!(matches!(
        change_set.repositories["shared"].snapshot,
        RepositorySnapshot::Unchanged {
            commits_ahead: 0,
            ..
        }
    ));

    let RepositorySnapshot::Changed {
        commits_ahead,
        files,
        insertions,
        deletions,
        diff,
        ..
    } = &change_set.repositories["api"].snapshot
    else {
        panic!("api should be changed");
    };
    assert_eq!(*commits_ahead, 0);
    assert!(*insertions >= 2);
    assert_eq!(*deletions, 0);
    assert_eq!(diff.sha256.len(), 64);
    assert!(!diff.truncated);
    assert_file(
        files,
        Path::new("README.md"),
        ChangedFileKind::Modified,
        false,
        true,
    );
    assert_file(
        files,
        Path::new("staged.txt"),
        ChangedFileKind::Added,
        true,
        false,
    );
    assert_file(files, odd_path, ChangedFileKind::Untracked, false, true);

    let RepositorySnapshot::Changed {
        commits_ahead,
        files,
        ..
    } = &change_set.repositories["web"].snapshot
    else {
        panic!("web should be changed");
    };
    assert_eq!(*commits_ahead, 1);
    assert_file(
        files,
        Path::new("WEB.md"),
        ChangedFileKind::Renamed,
        false,
        false,
    );
}

#[test]
fn parallel_tasks_report_only_changes_in_their_own_checkouts() {
    let fixture = ProjectFixture::new(false);
    let provisioner = TaskWorkspaceProvisioner::new(&fixture.states);
    for task_id in ["task-a", "task-b"] {
        fixture.create_task(task_id);
        provisioner
            .provision(
                &fixture.definition,
                &fixture.private_state,
                &fixture.project,
                task_id,
            )
            .unwrap();
    }
    let task_a = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "task-a",
            "api",
        )
        .unwrap();
    let task_b = provisioner
        .activate_repository(
            &fixture.definition,
            &fixture.private_state,
            &fixture.project,
            "task-b",
            "web",
        )
        .unwrap();
    std::fs::write(checkout(&task_a, "api").join("a.txt"), b"a\n").unwrap();
    std::fs::write(checkout(&task_b, "web").join("b.txt"), b"b\n").unwrap();

    let change_a = fixture
        .states
        .inspect_change_set(&fixture.project, "task-a")
        .unwrap();
    let change_b = fixture
        .states
        .inspect_change_set(&fixture.project, "task-b")
        .unwrap();

    assert_eq!(change_a.affected_repository_ids(), ["api"]);
    assert_eq!(change_b.affected_repository_ids(), ["web"]);
    assert_ne!(
        change_a.repositories["api"].checkout_path,
        change_b.repositories["api"].checkout_path
    );
}

fn checkout<'a>(task: &'a crate::project::task_workspace::TaskWorkspace, id: &str) -> &'a Path {
    &task.repositories[id]
        .worktree
        .as_ref()
        .unwrap()
        .checkout_path
}

fn assert_file(
    files: &[ChangedFile],
    path: &Path,
    kind: ChangedFileKind,
    staged: bool,
    worktree: bool,
) {
    let file = files.iter().find(|file| file.path == path).unwrap();
    assert_eq!(file.kind, kind);
    assert_eq!(file.staged, staged);
    assert_eq!(file.worktree, worktree);
}

fn run_git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}
