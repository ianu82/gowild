use super::harness::*;

#[test]
fn project_task_list_prints_route_recovery_and_paging_facts() {
    let base = unique_test_dir();
    let project = base.join("project");
    fs::create_dir_all(&project).unwrap();
    let socket_path = base.join("gowild.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let expected_path = project.canonicalize().unwrap();

    let server = thread::spawn({
        let socket_path = socket_path.clone();
        move || {
            let (mut stream, line) = accept_fake_cli_operation(&listener);
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "project.task.list");
            assert_eq!(request["params"]["path"], expected_path.to_str().unwrap());
            assert_eq!(request["params"]["after"], "older-task");
            assert_eq!(request["params"]["limit"], 25);

            let response = serde_json::json!({
                "id": "cli:project:task:list",
                "result": {
                    "type": "project_task_list",
                    "schema_version": 1,
                    "project": {
                        "project_id": "cowork",
                        "name": "MindsHub Cowork",
                        "root": expected_path,
                        "manifest_digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        "trust": "trusted"
                    },
                    "tasks": [{
                        "task_id": "route-settings",
                        "project_id": "cowork",
                        "outcome": "Add route settings",
                        "agent": "claude",
                        "route": {
                            "gateway_id": "mindshub",
                            "protocol": "anthropic_messages",
                            "model": "provider/team/very-long-model-id-with-routing-suffix"
                        },
                        "phase": "running",
                        "revision": 12,
                        "repository_count": 3,
                        "active_repository_count": 2,
                        "current_project": false,
                        "attention_code": "task_workspace_project_changed"
                    }],
                    "next_after": "route-settings"
                }
            });
            writeln!(stream, "{response}").unwrap();
            stream.flush().unwrap();
            let _ = fs::remove_file(socket_path);
        }
    });

    let output = run_cli(
        &socket_path,
        &[
            "project",
            "task",
            "list",
            project.to_str().unwrap(),
            "--after",
            "older-task",
            "--limit",
            "25",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("MindsHub Cowork (cowork)"));
    assert!(stdout.contains("Claude"));
    assert!(stdout.contains("provider/team/very-long-model-id-with-routing-suffix"));
    assert!(stdout.contains("needs attention: task_workspace_project_changed"));
    assert!(stdout.contains("Continue with --after route-settings"));

    server.join().unwrap();
    cleanup_test_base(&base);
}

#[test]
fn project_task_get_json_preserves_the_machine_readable_response() {
    let base = unique_test_dir();
    let project = base.join("project");
    fs::create_dir_all(&project).unwrap();
    let socket_path = base.join("gowild.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn({
        let socket_path = socket_path.clone();
        move || {
            let (mut stream, line) = accept_fake_cli_operation(&listener);
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "project.task.get");
            assert_eq!(request["params"]["task_id"], "route-settings");
            let response = serde_json::json!({
                "id": "cli:project:task:get",
                "result": {
                    "type": "project_task_info",
                    "schema_version": 1,
                    "marker": "machine-readable"
                }
            });
            writeln!(stream, "{response}").unwrap();
            stream.flush().unwrap();
            let _ = fs::remove_file(socket_path);
        }
    });

    let value = run_cli_json_in_dir(
        &socket_path,
        &["project", "task", "get", "route-settings", ".", "--json"],
        &project,
    );
    assert_eq!(value["result"]["marker"], "machine-readable");

    server.join().unwrap();
    cleanup_test_base(&base);
}

#[test]
fn project_task_human_errors_keep_the_structured_code() {
    let base = unique_test_dir();
    let project = base.join("project");
    fs::create_dir_all(&project).unwrap();
    let socket_path = base.join("gowild.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server = thread::spawn({
        let socket_path = socket_path.clone();
        move || {
            let (mut stream, _) = accept_fake_cli_operation(&listener);
            writeln!(
                stream,
                "{}",
                serde_json::json!({
                    "id": "cli:project:task:get",
                    "error": {
                        "code": "task_workspace_not_found",
                        "message": "task workspace 'missing' does not exist"
                    }
                })
            )
            .unwrap();
            stream.flush().unwrap();
            let _ = fs::remove_file(socket_path);
        }
    });

    let output = run_cli(
        &socket_path,
        &[
            "project",
            "task",
            "get",
            "missing",
            project.to_str().unwrap(),
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("project error [task_workspace_not_found]"));

    server.join().unwrap();
    cleanup_test_base(&base);
}
