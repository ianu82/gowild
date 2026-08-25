use crate::project::ProjectError;

pub(super) fn repository_selector_from_remote(remote: &str) -> Result<String, ProjectError> {
    let (host, path) = if let Ok(url) = url::Url::parse(remote) {
        let host = url.host_str().ok_or_else(invalid_remote)?.to_string();
        (host, url.path().trim_start_matches('/').to_string())
    } else {
        let (_, location) = remote.rsplit_once('@').unwrap_or(("", remote));
        let (host, path) = location.split_once(':').ok_or_else(invalid_remote)?;
        (host.to_string(), path.to_string())
    };
    let path = path.trim_end_matches(".git");
    let mut components = path.split('/');
    let owner = components.next().ok_or_else(invalid_remote)?;
    let repository = components.next().ok_or_else(invalid_remote)?;
    if components.next().is_some()
        || !safe_repository_component(owner)
        || !safe_repository_component(repository)
        || !safe_repository_component(&host)
    {
        return Err(invalid_remote());
    }
    let selector = if host.eq_ignore_ascii_case("github.com") {
        format!("{owner}/{repository}")
    } else {
        format!("{host}/{owner}/{repository}")
    };
    validate_repository_selector(&selector)?;
    Ok(selector)
}

pub(super) fn validate_repository_selector(selector: &str) -> Result<(), ProjectError> {
    let components = selector.split('/').collect::<Vec<_>>();
    if matches!(components.len(), 2 | 3)
        && components
            .iter()
            .all(|component| safe_repository_component(component))
    {
        Ok(())
    } else {
        Err(invalid_remote())
    }
}

pub(super) fn validate_remote_name(remote: &str) -> Result<(), ProjectError> {
    if safe_repository_component(remote) {
        Ok(())
    } else {
        Err(ProjectError::new(
            "task_change_set_invalid_remote",
            "Git remote name is not a safe identifier",
        ))
    }
}

fn safe_repository_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && !value.starts_with(['-', '.'])
        && !value.ends_with(['-', '.'])
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn invalid_remote() -> ProjectError {
    ProjectError::new(
        "task_change_set_invalid_github_remote",
        "task repository remote is not a supported GitHub repository",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_urls_resolve_to_explicit_github_repository_selectors() {
        assert_eq!(
            repository_selector_from_remote("git@github.com:owner/repo.git").unwrap(),
            "owner/repo"
        );
        assert_eq!(
            repository_selector_from_remote("https://ghe.example/owner/repo.git").unwrap(),
            "ghe.example/owner/repo"
        );
        assert_eq!(
            repository_selector_from_remote("ssh://git@ghe.example/owner/repo.git").unwrap(),
            "ghe.example/owner/repo"
        );
        for remote in ["/tmp/repo", "git@github.com:owner/repo/extra", "-bad"] {
            assert!(repository_selector_from_remote(remote).is_err());
        }
    }
}
