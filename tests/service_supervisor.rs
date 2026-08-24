use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

#[test]
fn supervised_child_writes_marker() {
    let Some(output) = std::env::var_os("GOWILD_SUPERVISED_CHILD_OUTPUT") else {
        return;
    };
    std::fs::write(output, b"service argv stayed direct\n").unwrap();
}

#[test]
fn hidden_supervisor_leases_before_start_and_runs_the_exact_child() {
    let root = test_root();
    std::fs::create_dir_all(&root).unwrap();
    let lease = root.join("lease.json");
    let start = root.join("start");
    let output = root.join("output.txt");
    let instance = "1234567890abcdef-service";
    let test_binary = std::env::current_exe().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_gowild"))
        .arg("__gowild-service-supervisor")
        .arg(instance)
        .arg(&lease)
        .arg(&start)
        .arg("--")
        .arg(test_binary)
        .args(["--exact", "supervised_child_writes_marker", "--nocapture"])
        .env("GOWILD_SUPERVISED_CHILD_OUTPUT", &output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    wait_for_path(&lease, &mut child);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&lease).unwrap()).unwrap();
    assert_eq!(record["version"], 1);
    assert_eq!(record["instance_id"], instance);
    assert_eq!(record["pid"], child.id());
    assert!(record["started_at_unix_millis"].as_u64().unwrap() > 0);
    assert!(!output.exists());

    std::fs::write(&start, b"stale-instance\n").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(!output.exists());
    std::fs::write(&start, format!("{instance}\n")).unwrap();

    let status = wait_for_exit(&mut child);
    assert!(status.success());
    assert_eq!(
        std::fs::read(&output).unwrap(),
        b"service argv stayed direct\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

fn wait_for_path(path: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("supervisor exited before leasing: {status}");
        }
        assert!(Instant::now() < deadline, "supervisor lease timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("supervisor did not exit after start");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn test_root() -> PathBuf {
    let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "gowild-service-supervisor-{}-{sequence}",
        std::process::id()
    ))
}
