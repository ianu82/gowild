use std::path::Path;

use crate::api::schema::{
    ErrorBody, ErrorResponse, ProjectTaskAgent, ProjectTaskChangeSetSummary,
    ProjectTaskCheckSummary, ProjectTaskGetParams, ProjectTaskInfo, ProjectTaskIsolationInfo,
    ProjectTaskListParams, ProjectTaskMergeGate, ProjectTaskPhase, ProjectTaskProjectInfo,
    ProjectTaskProtocol, ProjectTaskRepositoryInfo, ProjectTaskRouteInfo, ProjectTaskSummary,
    ProjectTaskTrust, ResponseResult, SuccessResponse, DEFAULT_PROJECT_TASK_PAGE_SIZE,
    PROJECT_TASK_API_VERSION,
};
use crate::project::change_set::{CheckStatus, MergeApproval, MergeGate};
use crate::project::task_workspace::{TaskAgent, TaskProtocol, TaskWorkspacePhase};
use crate::project::{ProjectError, ProjectTaskReader, ProjectTaskSnapshot, ProjectTrustStatus};

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

fn open_reader(path: &str) -> Result<ProjectTaskReader, ProjectError> {
    if path.is_empty() || path.contains('\0') {
        return Err(ProjectError::new(
            "invalid_project_path",
            "project path must not be empty or contain NUL bytes",
        ));
    }
    ProjectTaskReader::open(Path::new(path))
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
}
