use crate::api::schema::{
    ProjectTaskAgent, ProjectTaskChangeSetSummary, ProjectTaskInfo, ProjectTaskMergeGate,
    ProjectTaskPhase, ProjectTaskProjectInfo, ProjectTaskProtocol, ProjectTaskRecoveryAction,
    ProjectTaskSummary, ProjectTaskTrust,
};

pub(super) fn format_task_list(
    project: &ProjectTaskProjectInfo,
    tasks: &[ProjectTaskSummary],
    next_after: Option<&str>,
) -> String {
    let mut output = format!(
        "project: {} ({})\ntrust: {}\ntasks: {}\n",
        project.name,
        project.project_id,
        trust_label(project.trust),
        tasks.len()
    );
    if tasks.is_empty() {
        output.push_str("\nNo tasks yet.\n");
    } else {
        output.push('\n');
        for task in tasks {
            output.push_str(&format_task_summary(task));
        }
    }
    if let Some(next_after) = next_after {
        output.push_str(&format!(
            "\nMore tasks are available. Continue with --after {next_after}.\n"
        ));
    }
    output
}

pub(super) fn format_task_info(project: &ProjectTaskProjectInfo, task: &ProjectTaskInfo) -> String {
    let mut output = format!(
        "project: {} ({})\ntrust: {}\n\n{}",
        project.name,
        project.project_id,
        trust_label(project.trust),
        format_task_summary(&task.summary)
    );
    output.push_str("repositories:\n");
    for repository in &task.repositories {
        let state = match (
            repository.branch.as_deref(),
            repository.checkout_path.as_deref(),
        ) {
            (Some(branch), Some(path)) => format!("branch {branch} at {path}"),
            (None, Some(path)) => format!("detached at {path}"),
            _ => "not provisioned".to_string(),
        };
        output.push_str(&format!(
            "- {}: {}\n  source: {}\n  base: {}\n",
            repository.repository_id, state, repository.source_path, repository.base_commit
        ));
        if !repository.depends_on.is_empty() {
            output.push_str(&format!(
                "  depends on: {}\n",
                repository.depends_on.join(", ")
            ));
        }
    }
    let isolation = &task.isolation;
    output.push_str(&format!(
        "isolation:\n  namespace: {}\n  root: {}\n  temp: {}\n  cache: {}\n  data: {}\n  compose: {} ({})\n",
        isolation.namespace,
        isolation.root,
        isolation.temp,
        isolation.cache,
        isolation.data,
        if isolation.compose_enabled { "enabled" } else { "disabled" },
        isolation.compose_project,
    ));
    append_named_values(&mut output, "environment keys", &isolation.environment_keys);
    append_named_values(&mut output, "services", &isolation.declared_services);
    if !isolation.ports.is_empty() {
        let ports = isolation
            .ports
            .iter()
            .map(|(name, port)| format!("{name}={port}"))
            .collect::<Vec<_>>()
            .join(", ");
        output.push_str(&format!("  ports: {ports}\n"));
    }
    output
}

fn append_named_values(output: &mut String, label: &str, values: &[String]) {
    if !values.is_empty() {
        output.push_str(&format!("  {label}: {}\n", values.join(", ")));
    }
}

fn format_task_summary(task: &ProjectTaskSummary) -> String {
    let mut output = format!(
        "- {} — {}\n  {} · {} · {} / {} / {}\n  repositories: {}/{} active\n",
        task.task_id,
        task.outcome,
        phase_label(task.phase),
        agent_label(task.agent),
        task.route.gateway_id,
        protocol_label(task.route.protocol),
        task.route.model,
        task.active_repository_count,
        task.repository_count,
    );
    if !task.current_project {
        output.push_str(&format!(
            "  needs attention: {}\n",
            task.attention_code.as_deref().unwrap_or("project_changed")
        ));
    }
    if task.recovery.action != ProjectTaskRecoveryAction::None {
        output.push_str(&format!(
            "  recovery: {}\n",
            recovery_action_label(task.recovery.action)
        ));
    }
    if let Some(code) = &task.recovery.last_failure_code {
        output.push_str(&format!("  last failure: {code}\n"));
    }
    if let Some(change_set) = &task.change_set {
        output.push_str(&format_change_set(change_set));
    }
    output
}

fn recovery_action_label(action: ProjectTaskRecoveryAction) -> &'static str {
    match action {
        ProjectTaskRecoveryAction::None => "none",
        ProjectTaskRecoveryAction::Provision => "provision task workspace",
        ProjectTaskRecoveryAction::ResumeProvisioning => "resume interrupted provisioning",
        ProjectTaskRecoveryAction::ResumeCleanup => "resume interrupted cleanup",
        ProjectTaskRecoveryAction::ReconcileRuntime => "reconcile runtime ownership",
        ProjectTaskRecoveryAction::ReviewAttention => "review task attention state",
        ProjectTaskRecoveryAction::ReviewProjectDefinition => "review changed project definition",
    }
}

fn format_change_set(change_set: &ProjectTaskChangeSetSummary) -> String {
    format!(
        "  change set: {}/{} repositories affected · checks {} passed, {} failed, {} pending, {} skipped · {} planned PRs, {} drafts · {}{}\n",
        change_set.affected_repository_count,
        change_set.repository_count,
        change_set.checks.passed,
        change_set.checks.failed,
        change_set.checks.pending,
        change_set.checks.skipped,
        change_set.planned_pull_request_count,
        change_set.draft_pull_request_count,
        merge_gate_label(change_set.merge_gate),
        if change_set.stale { " · stale" } else { "" },
    )
}

fn trust_label(trust: ProjectTaskTrust) -> &'static str {
    match trust {
        ProjectTaskTrust::NotRequired => "not required",
        ProjectTaskTrust::Trusted => "trusted",
        ProjectTaskTrust::Untrusted => "untrusted",
        ProjectTaskTrust::Stale => "stale",
    }
}

fn agent_label(agent: ProjectTaskAgent) -> &'static str {
    match agent {
        ProjectTaskAgent::Codex => "Codex",
        ProjectTaskAgent::Claude => "Claude",
    }
}

fn protocol_label(protocol: ProjectTaskProtocol) -> &'static str {
    match protocol {
        ProjectTaskProtocol::OpenAiResponses => "OpenAI Responses",
        ProjectTaskProtocol::AnthropicMessages => "Anthropic Messages",
    }
}

fn phase_label(phase: ProjectTaskPhase) -> &'static str {
    match phase {
        ProjectTaskPhase::Planned => "planned",
        ProjectTaskPhase::Provisioning => "provisioning",
        ProjectTaskPhase::Ready => "ready",
        ProjectTaskPhase::Running => "running",
        ProjectTaskPhase::Stopped => "stopped",
        ProjectTaskPhase::Cleaning => "cleaning",
        ProjectTaskPhase::NeedsAttention => "needs attention",
        ProjectTaskPhase::Cleaned => "cleaned",
    }
}

fn merge_gate_label(gate: ProjectTaskMergeGate) -> &'static str {
    match gate {
        ProjectTaskMergeGate::AwaitingApproval => "awaiting approval",
        ProjectTaskMergeGate::ApprovedByHuman => "approved by human",
        ProjectTaskMergeGate::ApprovedByPolicy => "approved by policy",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        ProjectTaskCheckSummary, ProjectTaskIsolationInfo, ProjectTaskRecoveryInfo,
        ProjectTaskRepositoryInfo, ProjectTaskRouteInfo,
    };
    use std::collections::BTreeMap;

    #[test]
    fn task_list_keeps_full_route_and_stale_recovery_facts_visible() {
        let model = "provider/team/very-long-model-id-with-a-full-routing-suffix-2026-08-25";
        let mut task = task_summary(model);
        task.current_project = false;
        task.attention_code = Some("task_workspace_project_changed".into());
        let output = format_task_list(&project_info(), &[task], Some("route-settings"));

        assert!(output.contains(model));
        assert!(output.contains("Claude"));
        assert!(output.contains("Anthropic Messages"));
        assert!(output.contains("needs attention: task_workspace_project_changed"));
        assert!(output.contains("Continue with --after route-settings"));
    }

    #[test]
    fn task_detail_exposes_secret_names_but_has_no_value_channel() {
        let mut ports = BTreeMap::new();
        ports.insert("api".into(), 43123);
        let task = ProjectTaskInfo {
            summary: task_summary("provider/model"),
            task_schema_version: 1,
            manifest_digest: "a".repeat(64),
            root: "/tasks/route-settings".into(),
            repositories: vec![ProjectTaskRepositoryInfo {
                repository_id: "web".into(),
                source_path: "/projects/cowork/web".into(),
                base_commit: "b".repeat(40),
                depends_on: vec!["api".into()],
                checkout_path: Some("/tasks/route-settings/repos/web".into()),
                head_commit: Some("c".repeat(40)),
                branch: Some("gowild/route-settings/web".into()),
            }],
            isolation: ProjectTaskIsolationInfo {
                namespace: "cowork-route-settings".into(),
                root: "/tasks/route-settings/runtime".into(),
                temp: "/tasks/route-settings/runtime/tmp".into(),
                cache: "/tasks/route-settings/runtime/cache".into(),
                data: "/tasks/route-settings/runtime/data".into(),
                compose_project: "cowork-route-settings".into(),
                compose_enabled: true,
                environment_keys: vec!["MINDSHUB_API_KEY".into()],
                declared_services: vec!["api".into()],
                declared_ports: vec!["api".into()],
                declared_containers: Vec::new(),
                declared_databases: Vec::new(),
                declared_data: Vec::new(),
                declared_caches: Vec::new(),
                ports,
            },
        };
        let output = format_task_info(&project_info(), &task);

        assert!(output.contains("environment keys: MINDSHUB_API_KEY"));
        assert!(output.contains("ports: api=43123"));
        assert!(output.contains("branch gowild/route-settings/web"));
        assert!(!output.to_ascii_lowercase().contains("secret value"));
    }

    fn project_info() -> ProjectTaskProjectInfo {
        ProjectTaskProjectInfo {
            project_id: "cowork".into(),
            name: "MindsHub Cowork".into(),
            root: "/projects/cowork".into(),
            manifest_digest: "d".repeat(64),
            trust: ProjectTaskTrust::Trusted,
        }
    }

    fn task_summary(model: &str) -> ProjectTaskSummary {
        ProjectTaskSummary {
            task_id: "route-settings".into(),
            project_id: "cowork".into(),
            outcome: "Add route settings".into(),
            agent: ProjectTaskAgent::Claude,
            route: ProjectTaskRouteInfo {
                gateway_id: "mindshub".into(),
                protocol: ProjectTaskProtocol::AnthropicMessages,
                model: model.into(),
            },
            phase: ProjectTaskPhase::Running,
            revision: 12,
            repository_count: 3,
            active_repository_count: 2,
            current_project: true,
            attention_code: None,
            recovery: ProjectTaskRecoveryInfo {
                action: ProjectTaskRecoveryAction::ReconcileRuntime,
                interrupted: false,
                project_definition_changed: false,
                runtime_verification_required: true,
                pending_acquisitions: 0,
                pending_releases: 0,
                failed_acquisitions: 0,
                failed_releases: 0,
                owned_resource_count: 7,
                last_failure_code: None,
            },
            change_set: Some(ProjectTaskChangeSetSummary {
                record_revision: 4,
                task_revision: 12,
                stale: false,
                repository_count: 3,
                affected_repository_count: 2,
                checks: ProjectTaskCheckSummary {
                    passed: 2,
                    ..ProjectTaskCheckSummary::default()
                },
                planned_pull_request_count: 2,
                draft_pull_request_count: 0,
                merge_gate: ProjectTaskMergeGate::AwaitingApproval,
            }),
        }
    }
}
