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
        self.validate_journal()?;
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
        self.runtime
            .require_current_manifest(&project.manifest, &self.project_id, &self.id)
    }

    pub fn resource_is_owned(&self, resource: &OwnedResource) -> bool {
        let mut owned = false;
        for transition in &self.journal {
            if &transition.resource != resource || transition.state != TaskTransitionState::Applied
            {
                continue;
            }
            owned = transition.operation == TaskTransitionOperation::Acquire;
        }
        owned
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
            if self.root.starts_with(&state.source_path)
                || state.source_path.starts_with(&self.root)
            {
                return Err(ProjectError::new(
                    "task_workspace_repository_path_collision",
                    "task workspace data and source repositories must not contain one another",
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

    fn validate_journal(&self) -> Result<(), ProjectError> {
        if self.journal.len() > MAX_TRANSITIONS {
            return Err(ProjectError::new(
                "task_workspace_journal_full",
                "task workspace transition journal exceeds its safety limit",
            ));
        }
        let mut replay = self.clone();
        replay.journal.clear();
        replay.revision = 0;
        replay.runtime.ports.clear();
        for repository in replay.repositories.values_mut() {
            repository.worktree = None;
        }
        let mut owned = Vec::<OwnedResource>::new();
        let mut pending = Vec::<OwnedResource>::new();
        for (index, transition) in self.journal.iter().enumerate() {
            if transition.sequence != index as u64 + 1 {
                return Err(ProjectError::new(
                    "invalid_task_workspace_journal",
                    "task workspace transition sequence is not contiguous",
                ));
            }
            self.validate_resource(&transition.resource)?;
            match (transition.state, transition.failure_code.as_deref()) {
                (TaskTransitionState::Failed, Some(code)) => {
                    validate_identifier("transition failure code", code)?;
                }
                (TaskTransitionState::Failed, None) => {
                    return Err(ProjectError::new(
                        "missing_task_workspace_failure_code",
                        "a failed transition requires a stable failure code",
                    ));
                }
                (_, Some(_)) => {
                    return Err(ProjectError::new(
                        "unexpected_task_workspace_failure_code",
                        "only failed transitions may record a failure code",
                    ));
                }
                (_, None) => {}
            }
            if pending
                .iter()
                .any(|resource| resources_conflict(resource, &transition.resource))
            {
                return Err(ProjectError::new(
                    "duplicate_task_workspace_transition",
                    "task workspace journal has conflicting pending resources",
                ));
            }
            let is_owned = owned
                .iter()
                .any(|resource| resource == &transition.resource);
            match transition.operation {
                TaskTransitionOperation::Acquire
                    if owned
                        .iter()
                        .any(|resource| resources_conflict(resource, &transition.resource)) =>
                {
                    return Err(ProjectError::new(
                        "task_workspace_resource_collision",
                        "task workspace journal acquires a conflicting owned resource",
                    ));
                }
                TaskTransitionOperation::Release if !is_owned => {
                    return Err(ProjectError::new(
                        "unowned_task_workspace_resource",
                        "task workspace journal releases an unowned resource",
                    ));
                }
                _ => {}
            }
            replay.validate_operation_precondition(transition.operation, &transition.resource)?;
            if transition.state == TaskTransitionState::Planned {
                pending.push(transition.resource.clone());
                continue;
            }
            if transition.state != TaskTransitionState::Applied {
                continue;
            }
            replay.apply_transition_to_state(transition)?;
            match transition.operation {
                TaskTransitionOperation::Acquire => owned.push(transition.resource.clone()),
                TaskTransitionOperation::Release => {
                    let owned_index = owned
                        .iter()
                        .position(|resource| resource == &transition.resource)
                        .expect("release ownership was checked above");
                    owned.remove(owned_index);
                }
            }
        }
        if replay.repositories != self.repositories || replay.runtime.ports != self.runtime.ports {
            return Err(ProjectError::new(
                "task_workspace_journal_state_mismatch",
                "task workspace derived state does not match its transition journal",
            ));
        }
        if self.revision < self.journal.len() as u64 {
            return Err(ProjectError::new(
                "invalid_task_workspace_revision",
                "task workspace revision predates its transition journal",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_operation_precondition(
        &self,
        operation: TaskTransitionOperation,
        resource: &OwnedResource,
    ) -> Result<(), ProjectError> {
        match (operation, resource) {
            (
                TaskTransitionOperation::Acquire,
                OwnedResource::RepositoryWorktree { repository_id, .. },
            )
                if self.repositories[repository_id].worktree.is_some() =>
            {
                Err(ProjectError::new(
                    "task_workspace_resource_collision",
                    format!("repository '{repository_id}' already has a task worktree"),
                ))
            }
            (
                TaskTransitionOperation::Acquire,
                OwnedResource::RepositoryBranch { repository_id, .. },
            )
                if self.repositories[repository_id].worktree.is_none() =>
            {
                Err(ProjectError::new(
                    "task_workspace_worktree_not_ready",
                    format!("repository '{repository_id}' has no materialized worktree"),
                ))
            }
            (
                TaskTransitionOperation::Acquire,
                OwnedResource::PortReservation { name, port },
            )
                if self.runtime.ports.contains_key(name)
                    || self.runtime.ports.values().any(|existing| existing == port) =>
            {
                Err(ProjectError::new(
                    "task_workspace_port_collision",
                    format!("port reservation '{name}' collides with this task"),
                ))
            }
            (
                TaskTransitionOperation::Release,
                OwnedResource::RepositoryWorktree { repository_id, .. },
            ) if self.repositories[repository_id]
                .worktree
                .as_ref()
                .and_then(|worktree| worktree.branch.as_ref())
                .is_some() => Err(ProjectError::new(
                "task_workspace_branch_still_owned",
                format!(
                    "repository '{repository_id}' branch ownership must be released before its worktree"
                ),
            )),
            _ => Ok(()),
        }
    }

    pub(super) fn validate_resource(&self, resource: &OwnedResource) -> Result<(), ProjectError> {
        match resource {
            OwnedResource::WorkspaceDirectory { path } if path == &self.root => Ok(()),
            OwnedResource::RuntimeDirectory { path }
                if path == &self.runtime.root
                    || path == &self.runtime.temp
                    || path == &self.runtime.cache
                    || path == &self.runtime.data =>
            {
                Ok(())
            }
            OwnedResource::RepositoryWorktree {
                repository_id,
                source_path,
                checkout_path,
                base_commit,
            } => {
                let repository = self.repositories.get(repository_id).ok_or_else(|| {
                    ProjectError::new(
                        "unknown_task_workspace_repository",
                        format!("project has no repository '{repository_id}'"),
                    )
                })?;
                if source_path != &repository.source_path
                    || checkout_path != &self.repository_checkout_path(repository_id)
                    || base_commit != &repository.base_commit
                {
                    return Err(ProjectError::new(
                        "unowned_task_workspace_resource",
                        "worktree resource does not match this task's ownership boundary",
                    ));
                }
                Ok(())
            }
            OwnedResource::RepositoryBranch {
                repository_id,
                checkout_path,
                branch,
                base_commit,
            } => {
                let repository = self.repositories.get(repository_id).ok_or_else(|| {
                    ProjectError::new(
                        "unknown_task_workspace_repository",
                        format!("project has no repository '{repository_id}'"),
                    )
                })?;
                if checkout_path != &self.repository_checkout_path(repository_id)
                    || branch != &self.branch_name(repository_id)
                    || base_commit != &repository.base_commit
                {
                    return Err(ProjectError::new(
                        "unowned_task_workspace_resource",
                        "branch resource does not match this task's ownership boundary",
                    ));
                }
                Ok(())
            }
            OwnedResource::PortReservation { name, port } => {
                validate_identifier("port reservation name", name)?;
                if !self.runtime.declared_ports.contains(name) {
                    return Err(ProjectError::new(
                        "unknown_task_workspace_port",
                        format!("project does not declare port reservation '{name}'"),
                    ));
                }
                if *port == 0 {
                    return Err(ProjectError::new(
                        "invalid_task_workspace_port",
                        "port reservations cannot use port zero",
                    ));
                }
                Ok(())
            }
            OwnedResource::ComposeProject { name }
                if self.runtime.compose_enabled && name == &self.runtime.compose_project =>
            {
                Ok(())
            }
            OwnedResource::ServiceProcess {
                service_id,
                instance_id,
            } => {
                validate_identifier("service id", service_id)?;
                if !self.runtime.declared_services.contains(service_id) {
                    return Err(ProjectError::new(
                        "unknown_task_workspace_service",
                        format!("project does not declare service '{service_id}'"),
                    ));
                }
                if instance_id != &service_instance_id(&self.runtime.namespace, service_id) {
                    return Err(ProjectError::new(
                        "invalid_task_workspace_process",
                        "owned service process identity does not match this task namespace",
                    ));
                }
                Ok(())
            }
            _ => Err(ProjectError::new(
                "unowned_task_workspace_resource",
                "resource is outside this task's ownership boundary",
            )),
        }
    }

    pub(super) fn apply_transition_to_state(
        &mut self,
        transition: &TaskTransition,
    ) -> Result<(), ProjectError> {
        let acquire = transition.operation == TaskTransitionOperation::Acquire;
        match &transition.resource {
            OwnedResource::RepositoryWorktree {
                repository_id,
                checkout_path,
                base_commit,
                ..
            } => {
                let repository = self.repositories.get_mut(repository_id).ok_or_else(|| {
                    ProjectError::new(
                        "unknown_task_workspace_repository",
                        format!("project has no repository '{repository_id}'"),
                    )
                })?;
                if acquire {
                    repository.worktree = Some(TaskWorktree {
                        checkout_path: checkout_path.clone(),
                        head_commit: base_commit.clone(),
                        branch: None,
                    });
                } else {
                    repository.worktree = None;
                }
            }
            OwnedResource::RepositoryBranch {
                repository_id,
                branch,
                ..
            } => {
                let worktree = self
                    .repositories
                    .get_mut(repository_id)
                    .and_then(|repository| repository.worktree.as_mut())
                    .ok_or_else(|| {
                        ProjectError::new(
                            "task_workspace_worktree_not_ready",
                            format!("repository '{repository_id}' has no materialized worktree"),
                        )
                    })?;
                worktree.branch = acquire.then(|| branch.clone());
            }
            OwnedResource::PortReservation { name, port } => {
                if acquire {
                    if self.runtime.ports.values().any(|existing| existing == port) {
                        return Err(ProjectError::new(
                            "task_workspace_port_collision",
                            format!("port {port} is already reserved by this task"),
                        ));
                    }
                    self.runtime.ports.insert(name.clone(), *port);
                } else {
                    self.runtime.ports.remove(name);
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn bump_revision(&mut self) -> Result<(), ProjectError> {
        self.revision = self.revision.checked_add(1).ok_or_else(|| {
            ProjectError::new(
                "task_workspace_revision_exhausted",
                "task workspace revision counter is exhausted",
            )
        })?;
        Ok(())
    }
}
