use std::path::PathBuf;

use crate::project::resolve_project_definition;

use super::{load_private_project, report_error};

pub(super) fn run_override_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        print_override_help();
        return Ok(2);
    };
    match subcommand {
        "set-base" => set_base_override(&args[1..]),
        "clear-base" => clear_base_override(&args[1..]),
        "help" | "--help" | "-h" => {
            print_override_help();
            Ok(0)
        }
        _ => {
            print_override_help();
            Ok(2)
        }
    }
}

fn set_base_override(args: &[String]) -> std::io::Result<i32> {
    let (path, repo_id, base) = match parse_base_override_args(args, true) {
        Ok(values) => values,
        Err(code) => return Ok(code),
    };
    let Some(base) = base else {
        eprintln!("missing value for --base");
        return Ok(2);
    };
    let (definition, mut private_state, repository) = match load_private_project(&path) {
        Ok(context) => context,
        Err(error) => return Ok(report_error(error)),
    };
    let previous = private_state
        .overrides
        .repository_bases
        .insert(repo_id.clone(), base.clone());
    if previous.as_deref() == Some(base.as_str()) {
        println!("base override already set: {repo_id} → {base}");
        return Ok(0);
    }
    let trust_revoked = private_state.revoke_trust();
    if let Err(error) = resolve_project_definition(definition.clone(), &private_state.overrides) {
        return Ok(report_error(error));
    }
    if let Err(error) = repository.save(&definition, &private_state) {
        return Ok(report_error(error));
    }
    println!(
        "base override set: {repo_id} → {base}{}",
        trust_revocation_suffix(trust_revoked)
    );
    Ok(0)
}

fn clear_base_override(args: &[String]) -> std::io::Result<i32> {
    let (path, repo_id, _) = match parse_base_override_args(args, false) {
        Ok(values) => values,
        Err(code) => return Ok(code),
    };
    let (definition, mut private_state, repository) = match load_private_project(&path) {
        Ok(context) => context,
        Err(error) => return Ok(report_error(error)),
    };
    if private_state
        .overrides
        .repository_bases
        .remove(&repo_id)
        .is_none()
    {
        eprintln!("no base override exists for repository '{repo_id}'");
        return Ok(1);
    }
    let trust_revoked = private_state.revoke_trust();
    if let Err(error) = resolve_project_definition(definition.clone(), &private_state.overrides) {
        return Ok(report_error(error));
    }
    if let Err(error) = repository.save(&definition, &private_state) {
        return Ok(report_error(error));
    }
    println!(
        "base override cleared: {repo_id}{}",
        trust_revocation_suffix(trust_revoked)
    );
    Ok(0)
}

pub(super) fn run_trust(args: &[String]) -> std::io::Result<i32> {
    let (path, digest) = match parse_trust_args(args) {
        Ok(values) => values,
        Err(code) => return Ok(code),
    };
    let (definition, mut private_state, repository) = match load_private_project(&path) {
        Ok(context) => context,
        Err(error) => return Ok(report_error(error)),
    };
    if !definition.manifest.requires_trust() {
        println!("project has no executable manifest commands; trust is not required");
        return Ok(0);
    }
    if let Err(error) = private_state.grant_trust(&definition, &digest) {
        return Ok(report_error(error));
    }
    if let Err(error) = repository.save(&definition, &private_state) {
        return Ok(report_error(error));
    }
    println!("trusted project manifest digest {digest}");
    Ok(0)
}

pub(super) fn run_untrust(args: &[String]) -> std::io::Result<i32> {
    let path = match parse_path_only(args) {
        Ok(path) => path,
        Err(code) => return Ok(code),
    };
    let (definition, mut private_state, repository) = match load_private_project(&path) {
        Ok(context) => context,
        Err(error) => return Ok(report_error(error)),
    };
    if !private_state.revoke_trust() {
        println!("project manifest is already untrusted");
        return Ok(0);
    }
    if let Err(error) = repository.save(&definition, &private_state) {
        return Ok(report_error(error));
    }
    println!("project manifest trust revoked");
    Ok(0)
}

fn trust_revocation_suffix(revoked: bool) -> &'static str {
    if revoked {
        "; prior manifest trust revoked"
    } else {
        ""
    }
}

fn parse_path_only(args: &[String]) -> Result<PathBuf, i32> {
    match args {
        [] => Ok(PathBuf::from(".")),
        [path] if !path.starts_with('-') => Ok(PathBuf::from(path)),
        [option] => {
            eprintln!("unknown option: {option}");
            Err(2)
        }
        [_, unexpected, ..] => {
            eprintln!("unexpected argument: {unexpected}");
            Err(2)
        }
    }
}

fn parse_base_override_args(
    args: &[String],
    require_base: bool,
) -> Result<(PathBuf, String, Option<String>), i32> {
    let mut path = PathBuf::from(".");
    let mut path_set = false;
    let mut repo_id = None;
    let mut base = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--repo" | "--base" => {
                let flag = args[index].as_str();
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for {flag}");
                    return Err(2);
                };
                if flag == "--repo" {
                    repo_id = Some(value.clone());
                } else {
                    base = Some(value.clone());
                }
                index += 2;
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
    let Some(repo_id) = repo_id else {
        eprintln!("missing value for --repo");
        return Err(2);
    };
    if require_base && base.is_none() {
        eprintln!("missing value for --base");
        return Err(2);
    }
    if !require_base && base.is_some() {
        eprintln!("--base is not valid when clearing an override");
        return Err(2);
    }
    Ok((path, repo_id, base))
}

fn parse_trust_args(args: &[String]) -> Result<(PathBuf, String), i32> {
    let mut path = PathBuf::from(".");
    let mut path_set = false;
    let mut digest = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--digest" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --digest");
                    return Err(2);
                };
                digest = Some(value.clone());
                index += 2;
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
    let Some(digest) = digest else {
        eprintln!("missing value for --digest");
        return Err(2);
    };
    Ok((path, digest))
}

fn print_override_help() {
    eprintln!("gowild project override commands:");
    eprintln!("  gowild project override set-base [PATH] --repo ID --base REF");
    eprintln!("  gowild project override clear-base [PATH] --repo ID");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_override_arguments_are_unambiguous() {
        let args = [
            "./project".to_string(),
            "--repo".to_string(),
            "api".to_string(),
            "--base".to_string(),
            "release".to_string(),
        ];
        assert_eq!(
            parse_base_override_args(&args, true).unwrap(),
            (
                PathBuf::from("./project"),
                "api".to_string(),
                Some("release".to_string())
            )
        );

        assert!(parse_base_override_args(&args, false).is_err());
        assert!(parse_base_override_args(&args[..3], true).is_err());
    }

    #[test]
    fn trust_requires_an_explicit_digest() {
        assert!(parse_trust_args(&[]).is_err());
        assert_eq!(
            parse_trust_args(&[
                "./project".to_string(),
                "--digest".to_string(),
                "abc".to_string(),
            ])
            .unwrap(),
            (PathBuf::from("./project"), "abc".to_string())
        );
    }

    #[test]
    fn untrust_accepts_at_most_one_path() {
        assert_eq!(parse_path_only(&[]).unwrap(), PathBuf::from("."));
        assert_eq!(
            parse_path_only(&["./project".to_string()]).unwrap(),
            PathBuf::from("./project")
        );
        assert!(parse_path_only(&["--json".to_string()]).is_err());
        assert!(parse_path_only(&["one".to_string(), "two".to_string()]).is_err());
    }
}
