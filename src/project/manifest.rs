use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::model::{ProjectError, ProjectManifest, PROJECT_MANIFEST_FILE};
use super::overrides::ProjectOverrides;

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedProject {
    pub manifest_path: PathBuf,
    pub root: PathBuf,
    pub digest: String,
    pub manifest: ProjectManifest,
    pub repositories: Vec<ResolvedRepo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDefinition {
    pub manifest_path: PathBuf,
    pub root: PathBuf,
    pub digest: String,
    pub manifest: ProjectManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRepo {
    pub id: String,
    pub path: PathBuf,
    pub configured_base: Option<String>,
    pub base_commit: String,
    pub head_commit: String,
    pub depends_on: Vec<String>,
}

pub fn parse_manifest(contents: &str) -> Result<ProjectManifest, ProjectError> {
    let manifest = toml::from_str::<ProjectManifest>(contents).map_err(|error| {
        let location = error
            .span()
            .map(|span| format!(" near byte {}", span.start))
            .unwrap_or_default();
        ProjectError::new(
            "invalid_project_manifest_toml",
            format!("{PROJECT_MANIFEST_FILE} is not valid project TOML{location}"),
        )
    })?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn render_manifest(manifest: &ProjectManifest) -> Result<String, ProjectError> {
    manifest.validate()?;
    toml::to_string_pretty(manifest).map_err(|_| {
        ProjectError::new(
            "project_manifest_serialization_failed",
            format!("could not render {PROJECT_MANIFEST_FILE}"),
        )
    })
}

pub fn load_project(path: &Path) -> Result<LoadedProject, ProjectError> {
    load_project_with_overrides(path, &ProjectOverrides::default())
}

pub fn load_project_definition(path: &Path) -> Result<ProjectDefinition, ProjectError> {
    let manifest_path = if path.is_dir() {
        path.join(PROJECT_MANIFEST_FILE)
    } else {
        path.to_path_buf()
    };
    let metadata = fs::symlink_metadata(&manifest_path).map_err(|_| {
        ProjectError::new(
            "project_manifest_unavailable",
            format!("could not read {}", manifest_path.display()),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ProjectError::new(
            "unsafe_project_manifest",
            format!(
                "{} must be a regular file, not a symlink",
                manifest_path.display()
            ),
        ));
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(ProjectError::new(
            "project_manifest_too_large",
            format!("{PROJECT_MANIFEST_FILE} must be no larger than 1 MiB"),
        ));
    }

    let contents = fs::read_to_string(&manifest_path).map_err(|_| {
        ProjectError::new(
            "project_manifest_unavailable",
            format!("could not read {} as UTF-8", manifest_path.display()),
        )
    })?;
    let manifest = parse_manifest(&contents)?;
    let manifest_path = manifest_path.canonicalize().map_err(|_| {
        ProjectError::new(
            "project_manifest_unavailable",
            format!("could not resolve {}", manifest_path.display()),
        )
    })?;
    let root = manifest_path
        .parent()
        .ok_or_else(|| {
            ProjectError::new(
                "invalid_project_root",
                "project manifest has no parent directory",
            )
        })?
        .to_path_buf();

    Ok(ProjectDefinition {
        manifest_path,
        root,
        digest: sha256_hex(contents.as_bytes()),
        manifest,
    })
}

fn load_project_with_overrides(
    path: &Path,
    overrides: &ProjectOverrides,
) -> Result<LoadedProject, ProjectError> {
    let definition = load_project_definition(path)?;
    resolve_project_definition(definition, overrides)
}

pub fn resolve_project_definition(
    definition: ProjectDefinition,
    overrides: &ProjectOverrides,
) -> Result<LoadedProject, ProjectError> {
    overrides.validate_for(&definition.manifest)?;
    let mut manifest = definition.manifest.clone();
    overrides.apply_to(&mut manifest);
    manifest.validate()?;
    let root = &definition.root;
    let repositories = manifest
        .repositories
        .iter()
        .map(|repo| {
            let path = root.join(&repo.path).canonicalize().map_err(|_| {
                ProjectError::new(
                    "project_repository_unavailable",
                    format!(
                        "repository '{}' is unavailable at {}",
                        repo.id,
                        root.join(&repo.path).display()
                    ),
                )
            })?;
            if !path.starts_with(root) {
                return Err(ProjectError::new(
                    "project_repository_escape",
                    format!("repository '{}' resolves outside the project root", repo.id),
                ));
            }
            verify_git_root(&repo.id, &path)?;
            let head_commit = git_stdout(&repo.id, &path, &["rev-parse", "--verify", "HEAD"])?;
            let base_commit = match &repo.base {
                Some(base) => git_stdout(
                    &repo.id,
                    &path,
                    &[
                        "rev-parse",
                        "--verify",
                        "--end-of-options",
                        &format!("{base}^{{commit}}"),
                    ],
                )?,
                None => head_commit.clone(),
            };
            Ok(ResolvedRepo {
                id: repo.id.clone(),
                path,
                configured_base: repo.base.clone(),
                base_commit,
                head_commit,
                depends_on: repo.depends_on.clone(),
            })
        })
        .collect::<Result<Vec<_>, ProjectError>>()?;
    let mut resolved_paths = std::collections::BTreeSet::new();
    for repository in &repositories {
        if !resolved_paths.insert(repository.path.clone()) {
            return Err(ProjectError::new(
                "project_repository_collision",
                format!(
                    "repository '{}' resolves to the same Git checkout as another repository",
                    repository.id
                ),
            ));
        }
    }

    Ok(LoadedProject {
        manifest_path: definition.manifest_path,
        root: definition.root,
        digest: definition.digest,
        manifest,
        repositories,
    })
}

fn verify_git_root(id: &str, path: &Path) -> Result<(), ProjectError> {
    let top_level = PathBuf::from(git_stdout(id, path, &["rev-parse", "--show-toplevel"])?);
    let top_level = top_level.canonicalize().map_err(|_| {
        ProjectError::new(
            "project_repository_invalid",
            format!("repository '{id}' has an invalid Git top-level"),
        )
    })?;
    if top_level != path {
        return Err(ProjectError::new(
            "project_repository_not_root",
            format!("repository '{id}' path must identify its Git top-level"),
        ));
    }
    Ok(())
}

fn git_stdout(id: &str, path: &Path, args: &[&str]) -> Result<String, ProjectError> {
    let output = crate::noninteractive_process::command("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|_| {
            ProjectError::new(
                "project_git_unavailable",
                format!("could not inspect repository '{id}' with Git"),
            )
        })?;
    let stdout = String::from_utf8(output.stdout).ok();
    let stdout = stdout
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if output.status.success() {
        return stdout.map(str::to_string).ok_or_else(|| {
            ProjectError::new(
                "project_git_invalid_output",
                format!("Git returned no result for repository '{id}'"),
            )
        });
    }
    Err(ProjectError::new(
        "project_git_command_failed",
        format!("Git could not resolve repository '{id}' or its configured base"),
    ))
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::project::model::{ProjectCommand, ProjectRepo, PROJECT_MANIFEST_VERSION};
    use crate::project::overrides::ProjectOverrides;

    struct Fixture {
        root: PathBuf,
    }

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

    impl Fixture {
        fn new() -> Self {
            let fixture_id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "gowild-project-manifest-{}-{fixture_id}",
                std::process::id(),
            ));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn repo(&self, name: &str) {
            let path = self.root.join(name);
            fs::create_dir_all(&path).unwrap();
            run_git(&path, &["init", "--initial-branch=main"]);
            run_git(&path, &["config", "user.name", "GoWild Tests"]);
            run_git(&path, &["config", "user.email", "tests@gowild.invalid"]);
            run_git(&path, &["config", "commit.gpgSign", "false"]);
            fs::write(path.join("README.md"), format!("# {name}\n")).unwrap();
            run_git(&path, &["add", "README.md"]);
            run_git(&path, &["commit", "-m", "fixture"]);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn run_git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn three_repo_manifest() -> ProjectManifest {
        ProjectManifest {
            version: PROJECT_MANIFEST_VERSION,
            id: "commerce".into(),
            name: "Commerce".into(),
            repositories: vec![
                ProjectRepo {
                    id: "web".into(),
                    path: "web".into(),
                    base: Some("main".into()),
                    depends_on: vec!["shared".into()],
                },
                ProjectRepo {
                    id: "shared".into(),
                    path: "shared".into(),
                    base: Some("main".into()),
                    depends_on: Vec::new(),
                },
                ProjectRepo {
                    id: "api".into(),
                    path: "api".into(),
                    base: Some("main".into()),
                    depends_on: vec!["shared".into()],
                },
            ],
            setup: vec![ProjectCommand {
                id: "prepare".into(),
                repository: Some("api".into()),
                cwd: None,
                argv: vec!["just".into(), "prepare".into()],
                environment: BTreeMap::from([("MODE".into(), "test".into())]),
            }],
            tests: Vec::new(),
            services: Vec::new(),
        }
    }

    #[test]
    fn round_trips_and_loads_a_three_repo_project() {
        let fixture = Fixture::new();
        for repo in ["api", "shared", "web"] {
            fixture.repo(repo);
        }
        let rendered = render_manifest(&three_repo_manifest()).unwrap();
        fs::write(fixture.root.join(PROJECT_MANIFEST_FILE), &rendered).unwrap();

        let loaded = load_project(&fixture.root).unwrap();
        assert_eq!(
            loaded.manifest.dependency_order().unwrap(),
            ["shared", "web", "api"]
        );
        assert_eq!(loaded.repositories.len(), 3);
        assert!(loaded
            .repositories
            .iter()
            .all(|repo| repo.head_commit == repo.base_commit));
        assert_eq!(loaded.digest.len(), 64);
        assert_eq!(
            render_manifest(&parse_manifest(&rendered).unwrap()).unwrap(),
            rendered
        );
    }

    #[test]
    fn private_base_override_is_validated_and_resolved() {
        let fixture = Fixture::new();
        for repo in ["api", "shared", "web"] {
            fixture.repo(repo);
        }
        run_git(&fixture.root.join("api"), &["branch", "release"]);
        fs::write(
            fixture.root.join(PROJECT_MANIFEST_FILE),
            render_manifest(&three_repo_manifest()).unwrap(),
        )
        .unwrap();
        let overrides = ProjectOverrides {
            repository_bases: BTreeMap::from([("api".into(), "release".into())]),
        };

        let loaded = load_project_with_overrides(&fixture.root, &overrides).unwrap();
        let api = loaded
            .repositories
            .iter()
            .find(|repository| repository.id == "api")
            .unwrap();
        assert_eq!(api.configured_base.as_deref(), Some("release"));
        assert_eq!(api.base_commit, api.head_commit);
    }

    #[test]
    fn parse_error_does_not_echo_manifest_values() {
        let secret = "do-not-echo-this-value";
        let error = parse_manifest(&format!("version = {secret}")).unwrap_err();
        assert!(!error.message.contains(secret));
    }

    #[cfg(unix)]
    #[test]
    fn repository_symlink_may_not_escape_project_root() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let outside = Fixture::new();
        outside.repo("external");
        symlink(outside.root.join("external"), fixture.root.join("api")).unwrap();
        let mut manifest = three_repo_manifest();
        manifest.repositories = vec![ProjectRepo {
            id: "api".into(),
            path: "api".into(),
            base: Some("main".into()),
            depends_on: Vec::new(),
        }];
        fs::write(
            fixture.root.join(PROJECT_MANIFEST_FILE),
            render_manifest(&manifest).unwrap(),
        )
        .unwrap();

        let error = load_project(&fixture.root).unwrap_err();
        assert_eq!(error.code, "project_repository_escape");
    }

    #[cfg(unix)]
    #[test]
    fn repository_aliases_may_not_resolve_to_the_same_checkout() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.repo("api");
        symlink(fixture.root.join("api"), fixture.root.join("api-alias")).unwrap();
        let mut manifest = three_repo_manifest();
        manifest.repositories = vec![
            ProjectRepo {
                id: "api".into(),
                path: "api".into(),
                base: Some("main".into()),
                depends_on: Vec::new(),
            },
            ProjectRepo {
                id: "api-alias".into(),
                path: "api-alias".into(),
                base: Some("main".into()),
                depends_on: Vec::new(),
            },
        ];
        fs::write(
            fixture.root.join(PROJECT_MANIFEST_FILE),
            render_manifest(&manifest).unwrap(),
        )
        .unwrap();

        let error = load_project(&fixture.root).unwrap_err();
        assert_eq!(error.code, "project_repository_collision");
    }

    #[cfg(unix)]
    #[test]
    fn manifest_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let target = fixture.root.join("manifest-target.toml");
        fs::write(&target, render_manifest(&three_repo_manifest()).unwrap()).unwrap();
        symlink(&target, fixture.root.join(PROJECT_MANIFEST_FILE)).unwrap();
        let error = load_project(&fixture.root).unwrap_err();
        assert_eq!(error.code, "unsafe_project_manifest");
    }
}
