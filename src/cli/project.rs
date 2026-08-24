use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use crate::project::{
    discover_project, load_project_definition, render_manifest, resolve_project_definition,
    DiscoveryOptions, ProjectDefinition, ProjectError, ProjectPrivateState,
    ProjectPrivateStateRepository, PROJECT_MANIFEST_FILE,
};

pub(super) fn run_project_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        print_project_help();
        return Ok(2);
    };
    match subcommand {
        "discover" => discover(&args[1..]),
        "check" => check(&args[1..]),
        "state" => state(&args[1..]),
        "help" | "--help" | "-h" => {
            print_project_help();
            Ok(0)
        }
        _ => {
            print_project_help();
            Ok(2)
        }
    }
}

fn discover(args: &[String]) -> std::io::Result<i32> {
    let mut root = PathBuf::from(".");
    let mut root_set = false;
    let mut options = DiscoveryOptions::default();
    let mut write = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--id" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --id");
                    return Ok(2);
                };
                options.id = Some(value.clone());
                index += 2;
            }
            "--name" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --name");
                    return Ok(2);
                };
                options.name = Some(value.clone());
                index += 2;
            }
            "--write" => {
                write = true;
                index += 1;
            }
            value if value.starts_with('-') => {
                eprintln!("unknown option: {value}");
                return Ok(2);
            }
            value if !root_set => {
                root = PathBuf::from(value);
                root_set = true;
                index += 1;
            }
            value => {
                eprintln!("unexpected argument: {value}");
                return Ok(2);
            }
        }
    }

    let manifest = match discover_project(&root, options) {
        Ok(manifest) => manifest,
        Err(error) => return Ok(report_error(error)),
    };
    let rendered = match render_manifest(&manifest) {
        Ok(rendered) => rendered,
        Err(error) => return Ok(report_error(error)),
    };
    if write {
        let root = root.canonicalize()?;
        let destination = root.join(PROJECT_MANIFEST_FILE);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                eprintln!(
                    "{} already exists; GoWild left it unchanged",
                    destination.display()
                );
                return Ok(1);
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = file
            .write_all(rendered.as_bytes())
            .and_then(|()| file.sync_all())
        {
            let _ = std::fs::remove_file(&destination);
            return Err(error);
        }
        println!("created {}", destination.display());
    } else {
        print!("{rendered}");
    }
    Ok(0)
}

fn check(args: &[String]) -> std::io::Result<i32> {
    let (path, json) = match parse_path_and_json(args) {
        Ok(parsed) => parsed,
        Err(code) => return Ok(code),
    };
    let (definition, private_state, repository) = match load_private_project(&path) {
        Ok(context) => context,
        Err(error) => return Ok(report_error(error)),
    };
    let loaded = match resolve_project_definition(definition.clone(), &private_state.overrides) {
        Ok(project) => project,
        Err(error) => return Ok(report_error(error)),
    };
    let trust_status = private_state.trust_status(&definition);
    let execution_allowed = private_state.require_execution_trust(&definition).is_ok();
    if json {
        let repositories = loaded
            .repositories
            .iter()
            .map(|repo| {
                serde_json::json!({
                    "id": repo.id,
                    "path": repo.path,
                    "base": repo.configured_base,
                    "base_commit": repo.base_commit,
                    "head_commit": repo.head_commit,
                    "depends_on": repo.depends_on,
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::json!({
                "project": {
                    "id": loaded.manifest.id,
                    "name": loaded.manifest.name,
                    "root": loaded.root,
                    "manifest_path": loaded.manifest_path,
                    "manifest_digest": loaded.digest,
                    "dependency_order": loaded.manifest.dependency_order().unwrap_or_default(),
                    "requires_trust": loaded.manifest.requires_trust(),
                    "trust_status": trust_status.to_string(),
                    "project_commands_allowed": execution_allowed,
                    "private_state_path": repository.path_for(&definition),
                    "repository_base_overrides": &private_state.overrides.repository_bases,
                    "repositories": repositories,
                }
            })
        );
    } else {
        println!(
            "project: {} ({})\nrepositories: {}\ndependency order: {}\nmanifest: {}\ndigest: {}\ncommands require trust: {}\ntrust: {}\nproject commands allowed: {}\nprivate state: {}",
            loaded.manifest.name,
            loaded.manifest.id,
            loaded.repositories.len(),
            loaded
                .manifest
                .dependency_order()
                .unwrap_or_default()
                .join(" → "),
            loaded.manifest_path.display(),
            loaded.digest,
            yes_no(loaded.manifest.requires_trust()),
            trust_status,
            yes_no(execution_allowed),
            repository.path_for(&definition).display(),
        );
    }
    Ok(0)
}

fn state(args: &[String]) -> std::io::Result<i32> {
    let (path, json) = match parse_path_and_json(args) {
        Ok(parsed) => parsed,
        Err(code) => return Ok(code),
    };
    let (definition, private_state, repository) = match load_private_project(&path) {
        Ok(context) => context,
        Err(error) => return Ok(report_error(error)),
    };
    let trust_status = private_state.trust_status(&definition);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "project_id": definition.manifest.id,
                "manifest_digest": definition.digest,
                "path": repository.path_for(&definition),
                "repository_base_overrides": &private_state.overrides.repository_bases,
                "trust_status": trust_status.to_string(),
                "trusted_manifest_digest": private_state.trusted_manifest_digest(),
            })
        );
    } else {
        println!(
            "project: {}\nmanifest digest: {}\ntrust: {}\nbase overrides: {}\nprivate state: {}",
            definition.manifest.id,
            definition.digest,
            trust_status,
            private_state.overrides.repository_bases.len(),
            repository.path_for(&definition).display(),
        );
    }
    Ok(0)
}

fn load_private_project(
    path: &std::path::Path,
) -> Result<
    (
        ProjectDefinition,
        ProjectPrivateState,
        ProjectPrivateStateRepository,
    ),
    ProjectError,
> {
    let definition = load_project_definition(path)?;
    let repository = ProjectPrivateStateRepository::in_default_state_dir();
    let state = repository.load(&definition)?;
    Ok((definition, state, repository))
}

fn parse_path_and_json(args: &[String]) -> Result<(PathBuf, bool), i32> {
    let mut path = PathBuf::from(".");
    let mut path_set = false;
    let mut json = false;
    for argument in args {
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
    Ok((path, json))
}

fn report_error(error: ProjectError) -> i32 {
    eprintln!("project error [{}]: {error}", error.code);
    1
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn print_project_help() {
    eprintln!("gowild project commands:");
    eprintln!("  gowild project discover [PATH] [--id ID] [--name TEXT] [--write]");
    eprintln!("  gowild project check [PATH] [--json]");
    eprintln!("  gowild project state [PATH] [--json]");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_no_is_plain_language() {
        assert_eq!(yes_no(true), "yes");
        assert_eq!(yes_no(false), "no");
    }

    #[test]
    fn project_manifest_file_name_is_stable() {
        assert_eq!(
            std::path::Path::new(PROJECT_MANIFEST_FILE),
            std::path::Path::new("gowild-project.toml")
        );
    }
}
