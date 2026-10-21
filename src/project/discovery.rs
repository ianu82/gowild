use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::model::{ProjectError, ProjectManifest, ProjectRepo, PROJECT_MANIFEST_VERSION};

const IGNORED_DIRECTORIES: &[&str] = &[
    ".cache",
    ".git",
    ".gowild",
    ".idea",
    ".vscode",
    "node_modules",
    "target",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryOptions {
    pub id: Option<String>,
    pub name: Option<String>,
}

pub fn discover_project(
    root: &Path,
    options: DiscoveryOptions,
) -> Result<ProjectManifest, ProjectError> {
    let root = root.canonicalize().map_err(|_| {
        ProjectError::new(
            "project_discovery_root_unavailable",
            format!("could not resolve project folder {}", root.display()),
        )
    })?;
    if !root.is_dir() {
        return Err(ProjectError::new(
            "project_discovery_root_invalid",
            format!("project folder {} is not a directory", root.display()),
        ));
    }

    ensure_git_available()?;
    let mut candidates = vec![root.clone()];
    let entries = fs::read_dir(&root).map_err(|_| {
        ProjectError::new(
            "project_discovery_unavailable",
            format!("could not inspect project folder {}", root.display()),
        )
    })?;
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if name.starts_with('.') || IGNORED_DIRECTORIES.contains(&name.as_ref()) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && !file_type.is_symlink() {
            candidates.push(entry.path());
        }
    }
    candidates.sort();

    let mut repositories = Vec::new();
    let mut used_ids = BTreeSet::new();
    for candidate in candidates {
        let Some(top_level) = git_top_level(&candidate)? else {
            continue;
        };
        let Ok(candidate) = candidate.canonicalize() else {
            continue;
        };
        if top_level != candidate {
            continue;
        }
        let relative = candidate
            .strip_prefix(&root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| PathBuf::from("."));
        let relative = if relative.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative
        };
        let source_name = if relative == Path::new(".") {
            directory_name(&root)
        } else {
            directory_name(&candidate)
        };
        let id = unique_id(&slug(&source_name), &mut used_ids);
        repositories.push(ProjectRepo {
            id,
            path: relative,
            base: current_branch(&candidate)?,
            depends_on: Vec::new(),
        });
    }

    if repositories.is_empty() {
        return Err(ProjectError::new(
            "project_discovery_empty",
            "no Git repositories found at the project root or one directory below it",
        ));
    }

    let default_name = directory_name(&root);
    let manifest = ProjectManifest {
        version: PROJECT_MANIFEST_VERSION,
        id: options.id.unwrap_or_else(|| slug(&default_name)),
        name: options.name.unwrap_or(default_name),
        repositories,
        setup: Vec::new(),
        tests: Vec::new(),
        services: Vec::new(),
    };
    manifest.validate()?;
    Ok(manifest)
}

fn ensure_git_available() -> Result<(), ProjectError> {
    let output = crate::noninteractive_process::command("git")
        .arg("--version")
        .output()
        .map_err(|_| {
            ProjectError::new(
                "project_git_unavailable",
                "Git is required to discover project repositories",
            )
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(ProjectError::new(
            "project_git_unavailable",
            "Git is required to discover project repositories",
        ))
    }
}

fn git_top_level(candidate: &Path) -> Result<Option<PathBuf>, ProjectError> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(candidate)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|_| {
            ProjectError::new(
                "project_git_unavailable",
                "Git became unavailable while discovering repositories",
            )
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let Ok(stdout) = String::from_utf8(output.stdout) else {
        return Ok(None);
    };
    let path = PathBuf::from(stdout.trim());
    Ok(path.canonicalize().ok())
}

fn current_branch(repository: &Path) -> Result<Option<String>, ProjectError> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(repository)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .map_err(|_| {
            ProjectError::new(
                "project_git_unavailable",
                "Git became unavailable while reading repository branches",
            )
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let branch = String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(branch)
}

fn directory_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty() && !name.chars().any(char::is_control))
        .unwrap_or("GoWild project")
        .chars()
        .take(120)
        .collect()
}

fn slug(value: &str) -> String {
    let mut output = String::new();
    let mut separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !output.is_empty() {
                output.push('-');
            }
            output.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    output.truncate(63);
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        "project".to_string()
    } else {
        output
    }
}

fn unique_id(base: &str, used: &mut BTreeSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    for suffix in 2_u32.. {
        let suffix = format!("-{suffix}");
        let prefix_length = 63_usize.saturating_sub(suffix.len());
        let prefix = base.get(..base.len().min(prefix_length)).unwrap_or(base);
        let candidate = format!("{}{suffix}", prefix.trim_end_matches('-'));
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("an unbounded numeric suffix always produces a unique project id")
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    struct Fixture {
        root: PathBuf,
    }

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

    impl Fixture {
        fn new() -> Self {
            let fixture_id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "gowild-project-discovery-{}-{fixture_id}",
                std::process::id(),
            ));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn repo(&self, relative: &str) {
            let path = self.root.join(relative);
            fs::create_dir_all(&path).unwrap();
            let output = Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(["init", "--initial-branch=main"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git init failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn discovers_exactly_one_level_and_has_stable_order() {
        let fixture = Fixture::new();
        for repo in ["web", "shared", "api"] {
            fixture.repo(repo);
        }
        fixture.repo("api/nested");
        fs::create_dir_all(fixture.root.join("node_modules/ignored")).unwrap();

        let manifest = discover_project(&fixture.root, DiscoveryOptions::default()).unwrap();
        assert_eq!(
            manifest
                .repositories
                .iter()
                .map(|repo| (repo.id.as_str(), repo.path.as_path()))
                .collect::<Vec<_>>(),
            [
                ("api", Path::new("api")),
                ("shared", Path::new("shared")),
                ("web", Path::new("web")),
            ]
        );
        assert!(manifest
            .repositories
            .iter()
            .all(|repo| repo.base.as_deref() == Some("main")));
    }

    #[test]
    fn colliding_directory_slugs_are_disambiguated() {
        let fixture = Fixture::new();
        fixture.repo("web app");
        fixture.repo("web-app");

        let manifest = discover_project(&fixture.root, DiscoveryOptions::default()).unwrap();
        assert_eq!(
            manifest
                .repositories
                .iter()
                .map(|repo| repo.id.as_str())
                .collect::<Vec<_>>(),
            ["web-app", "web-app-2"]
        );
    }

    #[test]
    fn empty_folder_has_an_actionable_error() {
        let fixture = Fixture::new();
        let error = discover_project(&fixture.root, DiscoveryOptions::default()).unwrap_err();
        assert_eq!(error.code, "project_discovery_empty");
        assert!(error.message.contains("one directory below"));
    }
}
