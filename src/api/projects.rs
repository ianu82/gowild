use std::path::Path;

use crate::api::schema::{
    ErrorBody, ErrorResponse, ProjectTaskAgent, ProjectTaskChangeSetSummary,
    ProjectTaskCheckSummary, ProjectTaskCreateParams, ProjectTaskGetParams, ProjectTaskInfo,
    ProjectTaskIsolationInfo, ProjectTaskListParams, ProjectTaskMergeGate, ProjectTaskPhase,
    ProjectTaskProjectInfo, ProjectTaskProtocol, ProjectTaskRepositoryInfo, ProjectTaskRouteInfo,
    ProjectTaskSummary, ProjectTaskTrust, ResponseResult, SuccessResponse,
    DEFAULT_PROJECT_TASK_PAGE_SIZE, PROJECT_TASK_API_VERSION,
};
use crate::project::change_set::{CheckStatus, MergeApproval, MergeGate};
use crate::project::task_workspace::{TaskAgent, TaskProtocol, TaskRoute, TaskWorkspacePhase};
use crate::project::{
    CreateProjectTask, ProjectError, ProjectTaskReader, ProjectTaskService, ProjectTaskSnapshot,
    ProjectTrustStatus,
};

pub(super) fn task_list(id: String, params: ProjectTaskListParams) -> String {
    let limit = params.limit.unwrap_or(DEFAULT_PROJECT_TASK_PAGE_SIZE);
    if let Err(error) =
        ProjectTaskReader::validate_page(params.after.as_deref(), usize::from(limit))
    {
        return encode_error(id, error);
    }
    let reader = match open_reader(&params.path) {
        Ok(reader) => reader,
        Err(error) => return encode_error(id, error),
    };
    let page = match reader.list_page(params.after.as_deref(), usize::from(limit)) {
        Ok(page) => page,
        Err(error) => return encode_error(id, error),
    };
    let project = project_info(&reader);
    let tasks = page.tasks.iter().map(task_summary).collect();
    encode_success(
        id,
        ResponseResult::ProjectTaskList {
            schema_version: PROJECT_TASK_API_VERSION,
            project,
            tasks,
            next_after: page.next_after,
        },
    )
}

pub(super) fn task_get(id: String, params: ProjectTaskGetParams) -> String {
    if let Err(error) = ProjectTaskReader::validate_task_id(&params.task_id) {
        return encode_error(id, error);
    }
    let reader = match open_reader(&params.path) {
        Ok(reader) => reader,
        Err(error) => return encode_error(id, error),
    };
    let snapshot = match reader.get(&params.task_id) {
        Ok(snapshot) => snapshot,
        Err(error) => return encode_error(id, error),
    };
    let project = project_info(&reader);
    let task = task_info(&snapshot);
    encode_success(
        id,
        ResponseResult::ProjectTaskInfo {
            schema_version: PROJECT_TASK_API_VERSION,
            project,
            task,
        },
    )
}

pub(super) fn task_create(id: String, params: ProjectTaskCreateParams) -> String {
    let request = create_request(&params);
    if let Err(error) = request.validate() {
        return encode_error(id, error);
    }
    let path = match project_path(&params.path) {
        Ok(path) => path,
        Err(error) => return encode_error(id, error),
    };
    let service = match ProjectTaskService::open(path) {
        Ok(service) => service,
        Err(error) => return encode_error(id, error),
    };
    let task = match service.create(request) {
        Ok(task) => task,
        Err(error) => return encode_error(id, error),
    };
    let reader = service.reader();
    let snapshot = match reader.get(&task.id) {
        Ok(snapshot) => snapshot,
        Err(error) => return encode_error(id, error),
    };
    encode_success(
        id,
        ResponseResult::ProjectTaskInfo {
            schema_version: PROJECT_TASK_API_VERSION,
            project: project_info(&reader),
            task: task_info(&snapshot),
        },
    )
}

fn open_reader(path: &str) -> Result<ProjectTaskReader, ProjectError> {
    ProjectTaskReader::open(project_path(path)?)
}

fn project_path(path: &str) -> Result<&Path, ProjectError> {
    if path.is_empty() || path.contains('\0') {
        return Err(ProjectError::new(
            "invalid_project_path",
            "project path must not be empty or contain NUL bytes",
        ));
    }
    Ok(Path::new(path))
}

fn create_request(params: &ProjectTaskCreateParams) -> CreateProjectTask {
    CreateProjectTask {
        task_id: params.task_id.clone(),
        outcome: params.outcome.clone(),
        agent: match params.agent {
            ProjectTaskAgent::Codex => TaskAgent::Codex,
            ProjectTaskAgent::Claude => TaskAgent::Claude,
        },
        route: TaskRoute {
            gateway_id: params.route.gateway_id.clone(),
            protocol: match params.route.protocol {
                ProjectTaskProtocol::OpenAiResponses => TaskProtocol::OpenAiResponses,
                ProjectTaskProtocol::AnthropicMessages => TaskProtocol::AnthropicMessages,
            },
            model: params.route.model.clone(),
        },
    }
}

fn project_info(reader: &ProjectTaskReader) -> ProjectTaskProjectInfo {
    ProjectTaskProjectInfo {
        project_id: reader.project_id().to_string(),
        name: reader.project_name().to_string(),
        root: reader.project_root().to_string_lossy().into_owned(),
        manifest_digest: reader.manifest_digest().to_string(),
        trust: match reader.trust_status() {
            ProjectTrustStatus::NotRequired => ProjectTaskTrust::NotRequired,
            ProjectTrustStatus::Trusted => ProjectTaskTrust::Trusted,
            ProjectTrustStatus::Untrusted => ProjectTaskTrust::Untrusted,
            ProjectTrustStatus::Stale => ProjectTaskTrust::Stale,
        },
    }
}

fn task_info(snapshot: &ProjectTaskSnapshot) -> ProjectTaskInfo {
    let task = &snapshot.task;
    let repositories = task
        .repositories
        .iter()
        .map(|(repository_id, repository)| ProjectTaskRepositoryInfo {
            repository_id: repository_id.clone(),
            source_path: repository.source_path.to_string_lossy().into_owned(),
            base_commit: repository.base_commit.clone(),
            depends_on: repository.depends_on.clone(),
            checkout_path: repository
                .worktree
                .as_ref()
                .map(|worktree| worktree.checkout_path.to_string_lossy().into_owned()),
            head_commit: repository
                .worktree
                .as_ref()
                .map(|worktree| worktree.head_commit.clone()),
            branch: repository
                .worktree
                .as_ref()
                .and_then(|worktree| worktree.branch.clone()),
        })
        .collect();
    let isolation = &task.runtime;
    ProjectTaskInfo {
        summary: task_summary(snapshot),
        task_schema_version: task.schema_version,
        manifest_digest: task.manifest_digest.clone(),
        root: task.root.to_string_lossy().into_owned(),
        repositories,
        isolation: ProjectTaskIsolationInfo {
            namespace: isolation.namespace.clone(),
            root: isolation.root.to_string_lossy().into_owned(),
            temp: isolation.temp.to_string_lossy().into_owned(),
            cache: isolation.cache.to_string_lossy().into_owned(),
            data: isolation.data.to_string_lossy().into_owned(),
            compose_project: isolation.compose_project.clone(),
            compose_enabled: isolation.compose_enabled,
            environment_keys: isolation.environment.keys().cloned().collect(),
            declared_services: isolation.declared_services.iter().cloned().collect(),
            declared_ports: isolation.declared_ports.iter().cloned().collect(),
            declared_containers: isolation.declared_containers.iter().cloned().collect(),
            declared_databases: isolation.declared_databases.iter().cloned().collect(),
            declared_data: isolation.declared_data.iter().cloned().collect(),
            declared_caches: isolation.declared_caches.iter().cloned().collect(),
            ports: isolation.ports.clone(),
        },
    }
}

fn task_summary(snapshot: &ProjectTaskSnapshot) -> ProjectTaskSummary {
    let task = &snapshot.task;
    ProjectTaskSummary {
        task_id: task.id.clone(),
        project_id: task.project_id.clone(),
        outcome: task.outcome.clone(),
        agent: task_agent(task.agent),
        route: ProjectTaskRouteInfo {
            gateway_id: task.route.gateway_id.clone(),
            protocol: task_protocol(task.route.protocol),
            model: task.route.model.clone(),
        },
        phase: task_phase(task.phase),
        revision: task.revision,
        repository_count: task.repositories.len(),
        active_repository_count: task
            .repositories
            .values()
            .filter(|repository| repository.worktree.is_some())
            .count(),
        current_project: snapshot.current_project,
        attention_code: snapshot.attention_code.map(str::to_string),
        change_set: change_set_summary(snapshot),
    }
}

fn change_set_summary(snapshot: &ProjectTaskSnapshot) -> Option<ProjectTaskChangeSetSummary> {
    let change_set = snapshot.change_set.as_ref()?;
    let mut checks = ProjectTaskCheckSummary::default();
    for check in change_set.checks.values() {
        match check.status {
            CheckStatus::Pending => checks.pending += 1,
            CheckStatus::Passed => checks.passed += 1,
            CheckStatus::Failed => checks.failed += 1,
            CheckStatus::Skipped => checks.skipped += 1,
        }
    }
    Some(ProjectTaskChangeSetSummary {
        record_revision: snapshot.change_set_revision?,
        task_revision: change_set.task_revision,
        stale: snapshot.change_set_stale,
        repository_count: change_set.repositories.len(),
        affected_repository_count: change_set.affected_repository_ids().len(),
        checks,
        planned_pull_request_count: change_set.publication.planned_pull_requests.len(),
        draft_pull_request_count: change_set.publication.draft_pull_requests.len(),
        merge_gate: match &change_set.merge_gate {
            MergeGate::AwaitingApproval => ProjectTaskMergeGate::AwaitingApproval,
            MergeGate::Approved {
                approval: MergeApproval::Human { .. },
            } => ProjectTaskMergeGate::ApprovedByHuman,
            MergeGate::Approved {
                approval: MergeApproval::Policy { .. },
            } => ProjectTaskMergeGate::ApprovedByPolicy,
        },
    })
}

fn task_agent(agent: TaskAgent) -> ProjectTaskAgent {
    match agent {
        TaskAgent::Codex => ProjectTaskAgent::Codex,
        TaskAgent::Claude => ProjectTaskAgent::Claude,
    }
}

fn task_protocol(protocol: TaskProtocol) -> ProjectTaskProtocol {
    match protocol {
        TaskProtocol::OpenAiResponses => ProjectTaskProtocol::OpenAiResponses,
        TaskProtocol::AnthropicMessages => ProjectTaskProtocol::AnthropicMessages,
    }
}

fn task_phase(phase: TaskWorkspacePhase) -> ProjectTaskPhase {
    match phase {
        TaskWorkspacePhase::Planned => ProjectTaskPhase::Planned,
        TaskWorkspacePhase::Provisioning => ProjectTaskPhase::Provisioning,
        TaskWorkspacePhase::Ready => ProjectTaskPhase::Ready,
        TaskWorkspacePhase::Running => ProjectTaskPhase::Running,
        TaskWorkspacePhase::Stopped => ProjectTaskPhase::Stopped,
        TaskWorkspacePhase::Cleaning => ProjectTaskPhase::Cleaning,
        TaskWorkspacePhase::NeedsAttention => ProjectTaskPhase::NeedsAttention,
        TaskWorkspacePhase::Cleaned => ProjectTaskPhase::Cleaned,
    }
}

fn encode_success(id: String, result: ResponseResult) -> String {
    serde_json::to_string(&SuccessResponse { id, result }).unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"internal_error","message":"failed to encode response"}}"#
            .to_string()
    })
}

fn encode_error(id: String, error: ProjectError) -> String {
    serde_json::to_string(&ErrorResponse {
        id,
        error: ErrorBody {
            code: error.code.to_string(),
            message: error.message,
        },
    })
    .unwrap_or_else(|_| {
        r#"{"id":"","error":{"code":"internal_error","message":"failed to encode response"}}"#
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::task_workspace::provision_tests::ProjectFixture;

    struct StateHomeGuard(Option<std::ffi::OsString>);

    impl Drop for StateHomeGuard {
        fn drop(&mut self) {
            if let Some(value) = self.0.take() {
                std::env::set_var("XDG_STATE_HOME", value);
            } else {
                std::env::remove_var("XDG_STATE_HOME");
            }
        }
    }

    #[test]
    fn task_info_exposes_environment_names_but_never_values() {
        let fixture = ProjectFixture::new(false);
        let mut task = fixture.create_task("secret-safe");
        task.runtime
            .environment
            .insert("PRIVATE_TOKEN".into(), "do-not-expose".into());
        let snapshot = ProjectTaskSnapshot {
            task,
            current_project: true,
            attention_code: None,
            change_set_revision: None,
            change_set: None,
            change_set_stale: false,
        };

        let encoded = serde_json::to_string(&task_info(&snapshot)).unwrap();
        assert!(encoded.contains("PRIVATE_TOKEN"));
        assert!(!encoded.contains("do-not-expose"));
    }

    #[test]
    fn invalid_paths_are_rejected_without_dispatching_to_the_app() {
        for path in ["", "bad\0path"] {
            let response = task_list(
                "read".into(),
                ProjectTaskListParams {
                    path: path.into(),
                    after: None,
                    limit: None,
                },
            );
            let error = serde_json::from_str::<ErrorResponse>(&response).unwrap();
            assert_eq!(error.id, "read");
            assert_eq!(error.error.code, "invalid_project_path");

            let response = task_create("create".into(), create_params(path.into(), "safe-task"));
            let error = serde_json::from_str::<ErrorResponse>(&response).unwrap();
            assert_eq!(error.id, "create");
            assert_eq!(error.error.code, "invalid_project_path");
        }
    }

    #[test]
    fn invalid_page_inputs_are_rejected_before_filesystem_access() {
        for (after, limit, code) in [
            (None, Some(0), "invalid_project_task_page_size"),
            (Some("../escape"), Some(200), "invalid_project_task_cursor"),
        ] {
            let response = task_list(
                "page".into(),
                ProjectTaskListParams {
                    path: "/path/that/does/not/exist".into(),
                    after: after.map(str::to_string),
                    limit,
                },
            );
            let error = serde_json::from_str::<ErrorResponse>(&response).unwrap();
            assert_eq!(error.error.code, code);
        }
    }

    #[test]
    fn invalid_task_ids_are_rejected_before_filesystem_access() {
        let response = task_get(
            "task".into(),
            ProjectTaskGetParams {
                path: "/path/that/does/not/exist".into(),
                task_id: "../escape".into(),
            },
        );
        let error = serde_json::from_str::<ErrorResponse>(&response).unwrap();
        assert_eq!(error.error.code, "invalid_project_task_id");
    }

    #[test]
    fn task_create_is_planned_only_retry_safe_and_conflict_safe() {
        let _lock = crate::config::test_config_env_lock().lock().unwrap();
        let _state_home_guard = StateHomeGuard(std::env::var_os("XDG_STATE_HOME"));
        let fixture = ProjectFixture::new(true);
        std::env::set_var("XDG_STATE_HOME", fixture.root.join("api-create-state-home"));
        std::fs::write(
            &fixture.definition.manifest_path,
            crate::project::render_manifest(&fixture.definition.manifest).unwrap(),
        )
        .unwrap();
        let params = create_params(fixture.root.to_string_lossy().into_owned(), "api-create");

        let first = task_create("create".into(), params.clone());
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["result"]["type"], "project_task_info");
        assert_eq!(first["result"]["task"]["phase"], "planned");
        assert_eq!(first["result"]["task"]["revision"], 0);
        let task_root = first["result"]["task"]["root"].as_str().unwrap();
        assert!(!Path::new(task_root).exists());

        let retried = task_create("create".into(), params.clone());
        let retried: serde_json::Value = serde_json::from_str(&retried).unwrap();
        assert_eq!(retried, first);

        let mut conflicting = params;
        conflicting.outcome = "A different durable outcome".into();
        let conflict = task_create("conflict".into(), conflicting);
        let conflict: ErrorResponse = serde_json::from_str(&conflict).unwrap();
        assert_eq!(conflict.error.code, "task_workspace_already_exists");
        let stored = ProjectTaskReader::open(&fixture.root)
            .unwrap()
            .get("api-create")
            .unwrap();
        assert_eq!(
            stored.task.outcome,
            "Coordinate one change across every repository"
        );
    }

    #[test]
    fn task_create_rejects_all_domain_inputs_before_filesystem_access() {
        let path = "/project/path/that/does/not/exist".to_string();
        let mut cases = Vec::new();

        let invalid_id = create_params(path.clone(), "../escape");
        cases.push((invalid_id, "invalid_project_task_id"));

        let mut invalid_outcome = create_params(path.clone(), "safe-task");
        invalid_outcome.outcome.clear();
        cases.push((invalid_outcome, "invalid_task_workspace_outcome"));

        let mut invalid_gateway = create_params(path.clone(), "safe-task");
        invalid_gateway.route.gateway_id = "../gateway".into();
        cases.push((invalid_gateway, "invalid_task_workspace_identifier"));

        let mut invalid_model = create_params(path.clone(), "safe-task");
        invalid_model.route.model = "bad\0model".into();
        cases.push((invalid_model, "invalid_task_workspace_route"));

        let mut incompatible_protocol = create_params(path, "safe-task");
        incompatible_protocol.agent = ProjectTaskAgent::Claude;
        cases.push((incompatible_protocol, "invalid_task_workspace_route"));

        for (index, (params, expected_code)) in cases.into_iter().enumerate() {
            let response = task_create(format!("invalid-{index}"), params);
            let error: ErrorResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(error.error.code, expected_code);
        }
    }

    fn create_params(path: String, task_id: &str) -> ProjectTaskCreateParams {
        ProjectTaskCreateParams {
            path,
            task_id: task_id.into(),
            outcome: "Coordinate one change across every repository".into(),
            agent: ProjectTaskAgent::Codex,
            route: ProjectTaskRouteInfo {
                gateway_id: "mindshub".into(),
                protocol: ProjectTaskProtocol::OpenAiResponses,
                model: "provider/team/model".into(),
            },
        }
    }
}
