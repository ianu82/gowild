#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::project::change_set::{DraftPullRequestPlan, PullRequestState};

static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

#[test]
fn system_publisher_pushes_exact_branches_and_resumes_draft_reviews() {
    let fixture = PublicationFixture::new();
    let plan = fixture.plan();
    let mut publisher = fixture.publisher();

    let created = publisher
        .publish_draft(DraftPublicationRequest {
            checkout_path: &fixture.checkout,
            plan: &plan,
            existing: None,
        })
        .unwrap();
    assert_eq!(created.number, 41);
    assert_eq!(created.state, PullRequestState::Draft);
    assert_eq!(
        git_output(&fixture.remote, ["rev-parse", "refs/heads/gowild/task/api"]),
        git_output(&fixture.checkout, ["rev-parse", "HEAD"])
    );

    let adopted = publisher
        .publish_draft(DraftPublicationRequest {
            checkout_path: &fixture.checkout,
            plan: &plan,
            existing: None,
        })
        .unwrap();
    assert_eq!(adopted, created);

    let updated = publisher
        .publish_draft(DraftPublicationRequest {
            checkout_path: &fixture.checkout,
            plan: &plan,
            existing: Some(&created),
        })
        .unwrap();
    assert_eq!(updated, created);
    assert_eq!(std::fs::read_to_string(&fixture.body).unwrap(), plan.body);

    let log = std::fs::read_to_string(&fixture.log).unwrap();
    assert!(log.contains("ARG:create\n"));
    assert!(log.contains("ARG:--draft\n"));
    assert!(log.contains("ARG:edit\n"));
    assert!(log.contains("ARG:--body-file\nARG:-\n"));
    assert_eq!(log.matches("ARG:create\n").count(), 1);
    assert!(!log.contains("ARG:merge\n"));
    assert!(!log.contains("--force"));

    run_git(&fixture.checkout, ["reset", "--hard", "main"]);
    let error = publisher
        .publish_draft(DraftPublicationRequest {
            checkout_path: &fixture.checkout,
            plan: &plan,
            existing: Some(&created),
        })
        .unwrap_err();
    assert_eq!(error.code, "task_change_set_publication_command_failed");
}

struct PublicationFixture {
    root: PathBuf,
    remote: PathBuf,
    checkout: PathBuf,
    fake_gh: PathBuf,
    state: PathBuf,
    log: PathBuf,
    body: PathBuf,
}

impl PublicationFixture {
    fn new() -> Self {
        let sequence = NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "gowild-change-set-publisher-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        let remote = root.join("remote.git");
        let checkout = root.join("checkout");
        let fake_gh = root.join("fake-gh");
        let state = root.join("gh-state");
        let log = root.join("gh-log");
        let body = root.join("gh-body");
        std::fs::create_dir_all(&root).unwrap();

        run_git(&root, ["init", "--bare", remote.to_str().unwrap()]);
        run_git(
            &root,
            ["init", "--initial-branch=main", checkout.to_str().unwrap()],
        );
        run_git(
            &checkout,
            ["config", "user.email", "gowild@example.invalid"],
        );
        run_git(&checkout, ["config", "user.name", "GoWild Test"]);
        std::fs::write(checkout.join("README.md"), b"base\n").unwrap();
        run_git(&checkout, ["add", "README.md"]);
        run_git(&checkout, ["commit", "-m", "base"]);
        run_git(
            &checkout,
            ["remote", "add", "origin", remote.to_str().unwrap()],
        );
        run_git(&checkout, ["push", "--set-upstream", "origin", "main"]);
        run_git(&checkout, ["switch", "-c", "gowild/task/api"]);
        std::fs::write(checkout.join("api.txt"), b"change\n").unwrap();
        run_git(&checkout, ["add", "api.txt"]);
        run_git(&checkout, ["commit", "-m", "change api"]);

        std::fs::write(&fake_gh, FAKE_GH).unwrap();
        let mut permissions = std::fs::metadata(&fake_gh).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_gh, permissions).unwrap();

        Self {
            root,
            remote,
            checkout,
            fake_gh,
            state,
            log,
            body,
        }
    }

    fn plan(&self) -> DraftPullRequestPlan {
        DraftPullRequestPlan {
            repository_id: "api".into(),
            position: 1,
            head_branch: "gowild/task/api".into(),
            base_branch: "main".into(),
            title: "Update API".into(),
            body: "Coordinated review body\n".into(),
            depends_on: Vec::new(),
        }
    }

    fn publisher(&self) -> GitHubCliDraftPublisher {
        GitHubCliDraftPublisher::for_test(
            "origin",
            PathBuf::from("git"),
            self.fake_gh.clone(),
            "example/api",
            BTreeMap::from([
                (
                    OsString::from("FAKE_GH_STATE"),
                    self.state.as_os_str().to_owned(),
                ),
                (
                    OsString::from("FAKE_GH_LOG"),
                    self.log.as_os_str().to_owned(),
                ),
                (
                    OsString::from("FAKE_GH_BODY"),
                    self.body.as_os_str().to_owned(),
                ),
            ]),
        )
        .unwrap()
    }
}

impl Drop for PublicationFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run_git<I, S>(cwd: &Path, args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .unwrap();
    assert!(status.success());
}

fn git_output<I, S>(cwd: &Path, args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

const FAKE_GH: &str = r#"#!/bin/sh
set -eu
{
  printf 'BEGIN\n'
  for arg in "$@"; do
    printf 'ARG:%s\n' "$arg"
  done
  printf 'END\n'
} >> "$FAKE_GH_LOG"

case "$1:$2" in
  pr:list)
    if [ -f "$FAKE_GH_STATE" ]; then
      printf '%s\n' '[{"number":41,"url":"https://github.com/example/api/pull/41","state":"OPEN","isDraft":true,"baseRefName":"main","headRefName":"gowild/task/api","mergedAt":null}]'
    else
      printf '%s\n' '[]'
    fi
    ;;
  pr:create|pr:edit)
    cat > "$FAKE_GH_BODY"
    : > "$FAKE_GH_STATE"
    ;;
  *)
    exit 64
    ;;
esac
"#;
