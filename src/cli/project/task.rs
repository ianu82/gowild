use std::path::PathBuf;

use crate::api::schema::{
    Method, ProjectTaskGetParams, ProjectTaskListParams, Request, ResponseResult, SuccessResponse,
};

mod render;

use render::{format_task_info, format_task_list};

const MAX_TASK_PAGE_SIZE: u16 = 200;

pub(super) fn run(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("list") => task_list(&args[1..]),
        Some("get") => task_get(&args[1..]),
        Some("help" | "--help" | "-h") => {
            super::print_project_task_help();
            Ok(0)
        }
        _ => {
            super::print_project_task_help();
            Ok(2)
        }
    }
}

fn task_list(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_task_list_args(args) {
        Ok(options) => options,
        Err(code) => return Ok(code),
    };
    let path = canonical_api_path(&options.path)?;
    let response = super::super::send_request(&Request {
        id: "cli:project:task:list".into(),
        method: Method::ProjectTaskList(ProjectTaskListParams {
            path,
            after: options.after,
            limit: options.limit,
        }),
    })?;
    handle_project_response(response, options.json, |result| match result {
        ResponseResult::ProjectTaskList {
            project,
            tasks,
            next_after,
            ..
        } => {
            print!(
                "{}",
                format_task_list(&project, &tasks, next_after.as_deref())
            );
            Ok(())
        }
        result => Err(unexpected_project_result("project_task_list", &result)),
    })
}

fn task_get(args: &[String]) -> std::io::Result<i32> {
    let options = match parse_task_get_args(args) {
        Ok(options) => options,
        Err(code) => return Ok(code),
    };
    let path = canonical_api_path(&options.path)?;
    let response = super::super::send_request(&Request {
        id: "cli:project:task:get".into(),
        method: Method::ProjectTaskGet(ProjectTaskGetParams {
            path,
            task_id: options.task_id,
        }),
    })?;
    handle_project_response(response, options.json, |result| match result {
        ResponseResult::ProjectTaskInfo { project, task, .. } => {
            print!("{}", format_task_info(&project, &task));
            Ok(())
        }
        result => Err(unexpected_project_result("project_task_info", &result)),
    })
}

struct TaskListOptions {
    path: PathBuf,
    after: Option<String>,
    limit: Option<u16>,
    json: bool,
}

fn parse_task_list_args(args: &[String]) -> Result<TaskListOptions, i32> {
    let mut path = PathBuf::from(".");
    let mut path_set = false;
    let mut after = None;
    let mut limit = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--after" => {
                let Some(value) = args.get(index + 1).filter(|value| !value.starts_with('-'))
                else {
                    eprintln!("missing value for --after");
                    return Err(2);
                };
                after = Some(value.clone());
                index += 2;
            }
            "--limit" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --limit");
                    return Err(2);
                };
                let Some(parsed) = value
                    .parse::<u16>()
                    .ok()
                    .filter(|parsed| (1..=MAX_TASK_PAGE_SIZE).contains(parsed))
                else {
                    eprintln!("--limit must be an integer between 1 and {MAX_TASK_PAGE_SIZE}");
                    return Err(2);
                };
                limit = Some(parsed);
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
    Ok(TaskListOptions {
        path,
        after,
        limit,
        json,
    })
}

struct TaskGetOptions {
    task_id: String,
    path: PathBuf,
    json: bool,
}

fn parse_task_get_args(args: &[String]) -> Result<TaskGetOptions, i32> {
    let Some(task_id) = args.first() else {
        eprintln!("usage: gowild project task get <TASK_ID> [PATH] [--json]");
        return Err(2);
    };
    if task_id.starts_with('-') {
        eprintln!("missing TASK_ID");
        return Err(2);
    }
    let mut path = PathBuf::from(".");
    let mut path_set = false;
    let mut json = false;
    for argument in &args[1..] {
        match argument.as_str() {
            "--json" => json = true,
            value if value.starts_with('-') => {
                eprintln!("unknown option: {value}");
                return Err(2);
            }
            value if !path_set => {
                path = PathBuf::from(value);
                path_set = true;
            }
            value => {
                eprintln!("unexpected argument: {value}");
                return Err(2);
            }
        }
    }
    Ok(TaskGetOptions {
        task_id: task_id.clone(),
        path,
        json,
    })
}

fn canonical_api_path(path: &std::path::Path) -> std::io::Result<String> {
    let canonical = path.canonicalize()?;
    canonical
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| std::io::Error::other("project path is not valid UTF-8"))
}

fn handle_project_response(
    response: serde_json::Value,
    json: bool,
    render: impl FnOnce(ResponseResult) -> std::io::Result<()>,
) -> std::io::Result<i32> {
    if json {
        return super::super::print_response(&response);
    }
    if response.get("error").is_some() {
        let error = serde_json::from_value::<crate::api::schema::ErrorResponse>(response)
            .map_err(std::io::Error::other)?;
        eprintln!(
            "project error [{}]: {}",
            error.error.code, error.error.message
        );
        return Ok(1);
    }
    let success =
        serde_json::from_value::<SuccessResponse>(response).map_err(std::io::Error::other)?;
    render(success.result)?;
    Ok(0)
}

fn unexpected_project_result(expected: &str, _result: &ResponseResult) -> std::io::Error {
    std::io::Error::other(format!(
        "server returned an unexpected response; expected {expected}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_list_options_accept_path_paging_and_json_in_any_order() {
        let args = [
            "--limit",
            "50",
            "/projects/cowork",
            "--json",
            "--after",
            "first-task",
        ]
        .map(str::to_string);
        let parsed = parse_task_list_args(&args).unwrap();
        assert_eq!(parsed.path, PathBuf::from("/projects/cowork"));
        assert_eq!(parsed.limit, Some(50));
        assert_eq!(parsed.after.as_deref(), Some("first-task"));
        assert!(parsed.json);
    }

    #[test]
    fn task_list_options_reject_missing_cursors_and_unbounded_pages() {
        for args in [
            vec!["--after".to_string(), "--json".to_string()],
            vec!["--limit".to_string(), "0".to_string()],
            vec!["--limit".to_string(), "201".to_string()],
        ] {
            assert!(parse_task_list_args(&args).is_err());
        }
    }
}
