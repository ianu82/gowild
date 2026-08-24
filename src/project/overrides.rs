use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::model::{ProjectError, ProjectManifest};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectOverrides {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repository_bases: BTreeMap<String, String>,
}

impl ProjectOverrides {
    pub fn validate_for(&self, manifest: &ProjectManifest) -> Result<(), ProjectError> {
        let mut resolved = manifest.clone();
        for (repo_id, base) in &self.repository_bases {
            let Some(repository) = resolved
                .repositories
                .iter_mut()
                .find(|repository| &repository.id == repo_id)
            else {
                return Err(ProjectError::new(
                    "invalid_project_override",
                    format!("override references unknown repository '{repo_id}'"),
                ));
            };
            repository.base = Some(base.clone());
        }
        resolved.validate()
    }

    pub(super) fn apply_to(&self, manifest: &mut ProjectManifest) {
        for repository in &mut manifest.repositories {
            if let Some(base) = self.repository_bases.get(&repository.id) {
                repository.base = Some(base.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::{ProjectRepo, PROJECT_MANIFEST_VERSION};

    fn manifest() -> ProjectManifest {
        ProjectManifest {
            version: PROJECT_MANIFEST_VERSION,
            id: "sample".into(),
            name: "Sample".into(),
            repositories: vec![ProjectRepo {
                id: "api".into(),
                path: "api".into(),
                base: Some("main".into()),
                depends_on: Vec::new(),
            }],
            setup: Vec::new(),
            tests: Vec::new(),
            services: Vec::new(),
        }
    }

    #[test]
    fn unknown_repository_override_is_rejected() {
        let overrides = ProjectOverrides {
            repository_bases: BTreeMap::from([("missing".into(), "main".into())]),
        };

        assert_eq!(
            overrides.validate_for(&manifest()).unwrap_err().code,
            "invalid_project_override"
        );
    }

    #[test]
    fn unsafe_base_override_is_rejected_by_manifest_validation() {
        let overrides = ProjectOverrides {
            repository_bases: BTreeMap::from([("api".into(), "--upload-pack=evil".into())]),
        };

        assert_eq!(
            overrides.validate_for(&manifest()).unwrap_err().code,
            "invalid_project_manifest"
        );
    }
}
