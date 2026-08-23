use std::collections::BTreeMap;
use std::fmt;

use super::{CliAdapter, CodingCli};

#[derive(Default)]
pub(crate) struct AdapterRegistry {
    adapters: BTreeMap<CodingCli, Box<dyn CliAdapter>>,
}

impl AdapterRegistry {
    pub(crate) fn register(
        &mut self,
        adapter: impl CliAdapter + 'static,
    ) -> Result<(), RegistryError> {
        let cli = adapter.cli();
        if self.adapters.contains_key(&cli) {
            return Err(RegistryError::Duplicate(cli));
        }
        self.adapters.insert(cli, Box::new(adapter));
        Ok(())
    }

    pub(crate) fn get(&self, cli: CodingCli) -> Option<&dyn CliAdapter> {
        self.adapters.get(&cli).map(Box::as_ref)
    }

    pub(crate) fn configured_clis(&self) -> impl Iterator<Item = CodingCli> + '_ {
        self.adapters.keys().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistryError {
    Duplicate(CodingCli),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(cli) => write!(
                formatter,
                "an adapter for {} is already registered",
                cli.id()
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_adapter::{AdapterError, LaunchRequest, LaunchSpec, ResolvedGateway};
    use crate::gateway::GatewayProtocol;
    use std::path::Path;

    struct StubAdapter;

    impl CliAdapter for StubAdapter {
        fn cli(&self) -> CodingCli {
            CodingCli::Codex
        }

        fn display_name(&self) -> &'static str {
            "Stub Codex"
        }

        fn executable_candidates(&self) -> &'static [&'static str] {
            &["stub-codex"]
        }

        fn required_protocol(&self) -> GatewayProtocol {
            GatewayProtocol::OpenAiResponses
        }

        fn build(
            &self,
            executable: &Path,
            _resolved: &ResolvedGateway,
            _request: &LaunchRequest,
        ) -> Result<LaunchSpec, AdapterError> {
            Ok(LaunchSpec::new(
                CodingCli::Codex,
                executable.into(),
                Vec::new(),
                Default::default(),
            ))
        }
    }

    #[test]
    fn duplicate_cli_registration_is_rejected() {
        let mut registry = AdapterRegistry::default();
        registry.register(StubAdapter).unwrap();
        assert_eq!(
            registry.register(StubAdapter).unwrap_err(),
            RegistryError::Duplicate(CodingCli::Codex)
        );
        assert_eq!(
            registry.configured_clis().collect::<Vec<_>>(),
            vec![CodingCli::Codex]
        );
    }
}
