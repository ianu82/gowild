use std::path::PathBuf;

use crate::api::schema::{
    Method, ProjectTaskAgent, ProjectTaskCreateParams, ProjectTaskProtocol, ProjectTaskRouteInfo,
    Request, ResponseResult,
};

use super::render::format_task_info;
use super::{canonical_api_path, handle_project_response, unexpected_project_result};

pub(super) fn run(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(code) => return Ok(code),
    };
    let path = canonical_api_path(&options.path)?;
    let json = options.json;
    let response = super::super::super::send_request(&create_request(options, path))?;
    handle_project_response(response, json, |result| match result {
        ResponseResult::ProjectTaskInfo { project, task, .. } => {
            print!("{}", format_task_info(&project, &task));
            Ok(())
        }
        result => Err(unexpected_project_result("project_task_info", &result)),
    })
}

fn create_request(options: TaskCreateOptions, path: String) -> Request {
    let protocol = task_protocol(options.agent);
    Request {
        id: "cli:project:task:create".into(),
        method: Method::ProjectTaskCreate(ProjectTaskCreateParams {
            path,
            task_id: options.task_id,
            outcome: options.outcome,
            agent: options.agent,
            route: ProjectTaskRouteInfo {
                gateway_id: options.gateway_id,
                protocol,
                model: options.model,
            },
        }),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TaskCreateOptions {
    task_id: String,
    outcome: String,
    agent: ProjectTaskAgent,
    gateway_id: String,
    model: String,
    path: PathBuf,
    json: bool,
}

fn parse_args(args: &[String]) -> Result<TaskCreateOptions, i32> {
    let Some(task_id) = args.first() else {
        eprintln!(
            "usage: gowild project task create <TASK_ID> --outcome TEXT --agent <codex|claude> --gateway ID --model ID [PATH] [--json]"
        );
        return Err(2);
    };
    if task_id.starts_with('-') {
        eprintln!("missing TASK_ID");
        return Err(2);
    }

    let mut outcome = None;
    let mut agent = None;
    let mut gateway_id = None;
    let mut model = None;
    let mut path = PathBuf::from(".");
    let mut path_set = false;
    let mut json = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--outcome" => {
                set_once(
                    &mut outcome,
                    option_value(args, index, "--outcome")?,
                    "--outcome",
                )?;
                index += 2;
            }
            "--agent" => {
                set_once(&mut agent, option_value(args, index, "--agent")?, "--agent")?;
                index += 2;
            }
            "--gateway" => {
                set_once(
                    &mut gateway_id,
                    option_value(args, index, "--gateway")?,
                    "--gateway",
                )?;
                index += 2;
            }
            "--model" => {
                set_once(&mut model, option_value(args, index, "--model")?, "--model")?;
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                eprintln!("unknown option: {value}");
                return Err(2);
            }
            value if !path_set => {
                path = PathBuf::from(value);
                path_set = true;
                index += 1;
            }
            value => {
                eprintln!("unexpected argument: {value}");
                return Err(2);
            }
        }
    }

    let outcome = required_option(outcome, "--outcome")?;
    let agent = match required_option(agent, "--agent")?.as_str() {
        "codex" => ProjectTaskAgent::Codex,
        "claude" => ProjectTaskAgent::Claude,
        value => {
            eprintln!("--agent must be codex or claude, not {value}");
            return Err(2);
        }
    };
    Ok(TaskCreateOptions {
        task_id: task_id.clone(),
        outcome,
        agent,
        gateway_id: required_option(gateway_id, "--gateway")?,
        model: required_option(model, "--model")?,
        path,
        json,
    })
}

fn option_value(args: &[String], index: usize, name: &str) -> Result<String, i32> {
    let Some(value) = args.get(index + 1).filter(|value| !value.starts_with('-')) else {
        eprintln!("missing value for {name}");
        return Err(2);
    };
    Ok(value.clone())
}

fn set_once(slot: &mut Option<String>, value: String, name: &str) -> Result<(), i32> {
    if slot.replace(value).is_some() {
        eprintln!("{name} may only be provided once");
        return Err(2);
    }
    Ok(())
}

fn required_option(value: Option<String>, name: &str) -> Result<String, i32> {
    value.ok_or_else(|| {
        eprintln!("missing required option {name}");
        2
    })
}

fn task_protocol(agent: ProjectTaskAgent) -> ProjectTaskProtocol {
    match agent {
        ProjectTaskAgent::Codex => ProjectTaskProtocol::OpenAiResponses,
        ProjectTaskAgent::Claude => ProjectTaskProtocol::AnthropicMessages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_preserve_the_full_route_and_infer_protocol_from_the_agent() {
        let args = [
            "route-settings",
            "--model",
            "provider/team/full-model-id",
            "/projects/cowork",
            "--agent",
            "claude",
            "--gateway",
            "mindshub",
            "--outcome",
            "Add route settings across every repository",
            "--json",
        ]
        .map(str::to_string);
        let parsed = parse_args(&args).unwrap();

        assert_eq!(parsed.task_id, "route-settings");
        assert_eq!(parsed.agent, ProjectTaskAgent::Claude);
        assert_eq!(parsed.gateway_id, "mindshub");
        assert_eq!(parsed.model, "provider/team/full-model-id");
        assert_eq!(parsed.path, PathBuf::from("/projects/cowork"));
        assert!(parsed.json);
        assert_eq!(
            task_protocol(parsed.agent),
            ProjectTaskProtocol::AnthropicMessages
        );
        assert_eq!(
            task_protocol(ProjectTaskAgent::Codex),
            ProjectTaskProtocol::OpenAiResponses
        );

        let request =
            serde_json::to_value(create_request(parsed, "/canonical/projects/cowork".into()))
                .unwrap();
        assert_eq!(request["method"], "project.task.create");
        assert_eq!(request["params"]["path"], "/canonical/projects/cowork");
        assert_eq!(request["params"]["route"]["protocol"], "anthropic_messages");
        assert_eq!(
            request["params"]["route"]["model"],
            "provider/team/full-model-id"
        );
    }

    #[test]
    fn options_reject_missing_invalid_and_duplicate_values() {
        for args in [
            vec!["route-settings".to_string()],
            [
                "route-settings",
                "--outcome",
                "Outcome",
                "--agent",
                "other",
                "--gateway",
                "mindshub",
                "--model",
                "model",
            ]
            .map(str::to_string)
            .to_vec(),
            [
                "route-settings",
                "--outcome",
                "Outcome",
                "--outcome",
                "Other",
                "--agent",
                "codex",
                "--gateway",
                "mindshub",
                "--model",
                "model",
            ]
            .map(str::to_string)
            .to_vec(),
        ] {
            assert!(parse_args(&args).is_err());
        }
    }
}
