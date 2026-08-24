use super::rules::*;
use super::*;
use std::collections::BTreeSet;

impl TaskWorkspace {
    pub fn validate(&self, project: &LoadedProject) -> Result<(), ProjectError> {
        self.validate_integrity()?;
        self.require_current_project(project)
    }

    /// Validates the ownership record without trusting the current manifest.
    /// Recovery and cleanup may use this after a manifest changes; launching
    /// project commands may not.
    pub fn validate_integrity(&self) -> Result<(), ProjectError> {
        if self.schema_version != TASK_WORKSPACE_VERSION {
            return Err(ProjectError::new(
                "unsupported_task_workspace_version",
                format!(
                    "task workspace version {} is not supported",
                    self.schema_version
                ),
            ));
        }
        validate_identifier("task id", &self.id)?;
        validate_identifier("project id", &self.project_id)?;
        validate_digest("project manifest digest", &self.manifest_digest, 64)?;
        validate_outcome(&self.outcome)?;
        self.route.validate(self.agent)?;
        validate_absolute_clean_path("task workspace root", &self.root)?;
        let expected_namespace =
            runtime_namespace(&self.project_id, &self.id, &self.manifest_digest);
        if self.runtime.namespace != expected_namespace
            || self.root.file_name().and_then(|name| name.to_str())
                != Some(expected_namespace.as_str())
            || self
                .root
                .parent()
                .is_none_or(|parent| parent.parent().is_none())
        {
            return Err(ProjectError::new(
                "invalid_task_workspace_path",
                "task workspace root is not inside a dedicated store root",
            ));
        }
        self.runtime
            .validate(&self.root, &self.project_id, &self.id)?;
        self.validate_repository_integrity()?;
        Ok(())
    }

    /// Execution gate for immutable project inputs. A stale task remains
    /// inspectable and cleanable through `validate_integrity`.
    pub fn require_current_project(&self, project: &LoadedProject) -> Result<(), ProjectError> {
        if self.project_id != project.manifest.id || self.manifest_digest != project.digest {
            return Err(ProjectError::new(
                "task_workspace_project_mismatch",
                "task workspace belongs to a different project definition",
            ));
        }
        self.validate_repositories_against_project(project)?;
        self.runtime.require_current_manifest(&project.manifest)
    }

    fn validate_repository_integrity(&self) -> Result<(), ProjectError> {
        if self.repositories.is_empty() {
            return Err(ProjectError::new(
                "task_workspace_repository_mismatch",
                "task workspace must contain at least one repository",
            ));
        }
        let repository_ids = self
            .repositories
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let mut source_paths = BTreeSet::new();
        for (repository_id, state) in &self.repositories {
            validate_identifier("repository id", repository_id)?;
            validate_absolute_clean_path("repository source path", &state.source_path)?;
            if !source_paths.insert(&state.source_path) {
                return Err(ProjectError::new(
                    "task_workspace_repository_mismatch",
                    "task workspace repositories repeat a source path",
                ));
            }
            validate_git_object_id(&state.base_commit)?;
            let mut dependencies = BTreeSet::new();
            for dependency in &state.depends_on {
                if dependency == repository_id
                    || !repository_ids.contains(dependency.as_str())
                    || !dependencies.insert(dependency)
                {
                    return Err(ProjectError::new(
                        "task_workspace_repository_mismatch",
                        format!(
                            "task workspace repository '{repository_id}' has invalid dependencies"
                        ),
                    ));
                }
            }
            self.validate_task_worktree(repository_id, state)?;
        }
        validate_dependency_graph(&self.repositories)
    }

    fn validate_repositories_against_project(
        &self,
        project: &LoadedProject,
    ) -> Result<(), ProjectError> {
        let expected = project
            .repositories
            .iter()
            .map(|repository| repository.id.as_str())
            .collect::<BTreeSet<_>>();
        let actual = self
            .repositories
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if actual != expected {
            return Err(ProjectError::new(
                "task_workspace_repository_mismatch",
                "task workspace repository set does not match the project definition",
            ));
        }
        for repository in &project.repositories {
            let state = &self.repositories[&repository.id];
            if state.source_path != repository.path
                || state.base_commit != repository.base_commit
                || state.depends_on != repository.depends_on
            {
                return Err(ProjectError::new(
                    "task_workspace_repository_mismatch",
                    format!(
                        "task workspace repository '{}' no longer matches the project definition",
                        repository.id
                    ),
                ));
            }
        }
        Ok(())
    }

    fn validate_task_worktree(
        &self,
        repository_id: &str,
        state: &TaskRepository,
    ) -> Result<(), ProjectError> {
        let Some(worktree) = &state.worktree else {
            return Ok(());
        };
        if worktree.checkout_path != self.repository_checkout_path(repository_id) {
            return Err(ProjectError::new(
                "task_workspace_checkout_escape",
                format!("repository '{repository_id}' checkout is outside its owned location"),
            ));
        }
        validate_git_object_id(&worktree.head_commit)?;
        if worktree
            .branch
            .as_ref()
            .is_some_and(|branch| branch != &self.branch_name(repository_id))
        {
            return Err(ProjectError::new(
                "task_workspace_branch_mismatch",
                format!("repository '{repository_id}' branch is not owned by this task"),
            ));
        }
        Ok(())
    }
}
