use std::fmt;

use crate::gateway::{
    AuthenticationMode, Credential, CredentialStore, Gateway, GatewayCatalog, GatewayProtocol,
};

pub(crate) const ENV_GATEWAY: &str = "GOWILD_GATEWAY";
pub(crate) const ENV_MODEL: &str = "GOWILD_MODEL";
pub(crate) const ENV_API_KEY: &str = "GOWILD_API_KEY";
pub(crate) const ENV_RESPONSES_BASE_URL: &str = "GOWILD_RESPONSES_BASE_URL";
pub(crate) const ENV_MESSAGES_BASE_URL: &str = "GOWILD_MESSAGES_BASE_URL";

pub(crate) trait Environment {
    fn get(&self, key: &str) -> Option<String>;
}

pub(crate) struct GatewayResolver<'a> {
    catalog: &'a GatewayCatalog,
    credentials: &'a dyn CredentialStore,
    environment: &'a dyn Environment,
}

impl<'a> GatewayResolver<'a> {
    pub(crate) fn new(
        catalog: &'a GatewayCatalog,
        credentials: &'a dyn CredentialStore,
        environment: &'a dyn Environment,
    ) -> Self {
        Self {
            catalog,
            credentials,
            environment,
        }
    }

    pub(crate) fn resolve(
        &self,
        cli_id: &str,
        protocol: GatewayProtocol,
        explicit_gateway_id: Option<&str>,
        explicit_model: Option<&str>,
    ) -> Result<ResolvedGateway, GatewayResolutionError> {
        let gateway_id = nonempty(explicit_gateway_id.map(str::to_string))
            .or_else(|| nonempty(self.environment.get(ENV_GATEWAY)))
            .or_else(|| self.catalog.default_gateway_id.clone())
            .ok_or(GatewayResolutionError::NoDefaultGateway)?;

        let mut gateway = self
            .catalog
            .gateways
            .get(&gateway_id)
            .cloned()
            .ok_or(GatewayResolutionError::GatewayNotFound)?;

        if let Some(endpoint) = nonempty(self.environment.get(ENV_RESPONSES_BASE_URL)) {
            gateway.endpoints.openai_responses = Some(endpoint);
        }
        if let Some(endpoint) = nonempty(self.environment.get(ENV_MESSAGES_BASE_URL)) {
            gateway.endpoints.anthropic_messages = Some(endpoint);
        }
        let validation = gateway.validate();
        if !validation.is_empty() {
            return Err(GatewayResolutionError::InvalidGateway(validation));
        }
        if !gateway.supports(protocol) {
            return Err(GatewayResolutionError::UnsupportedProtocol(protocol));
        }

        let model = nonempty(explicit_model.map(str::to_string))
            .or_else(|| nonempty(self.environment.get(ENV_MODEL)))
            .or_else(|| gateway.default_models.get(cli_id).cloned());
        if model.as_deref().is_some_and(invalid_model) {
            return Err(GatewayResolutionError::InvalidModel);
        }

        let environment_credential = nonempty(self.environment.get(ENV_API_KEY));
        let credential = match gateway.auth.mode {
            AuthenticationMode::None => {
                if environment_credential.is_some() {
                    return Err(GatewayResolutionError::UnexpectedCredential);
                }
                None
            }
            _ => {
                if let Some(value) = environment_credential {
                    Some(
                        Credential::new(value)
                            .map_err(|_| GatewayResolutionError::InvalidCredential)?,
                    )
                } else {
                    let credential_ref = gateway
                        .auth
                        .credential_ref
                        .as_deref()
                        .ok_or(GatewayResolutionError::MissingCredential)?;
                    self.credentials
                        .get(credential_ref)
                        .map_err(|_| GatewayResolutionError::CredentialStoreUnavailable)?
                        .ok_or(GatewayResolutionError::MissingCredential)
                        .map(Some)?
                }
            }
        };

        let endpoint = gateway
            .endpoints
            .for_protocol(protocol)
            .expect("validated protocol endpoint must exist")
            .to_string();
        Ok(ResolvedGateway {
            gateway,
            protocol,
            endpoint,
            credential,
            model,
        })
    }
}

#[derive(Clone)]
pub(crate) struct ResolvedGateway {
    pub(crate) gateway: Gateway,
    pub(crate) protocol: GatewayProtocol,
    pub(crate) endpoint: String,
    pub(crate) credential: Option<Credential>,
    pub(crate) model: Option<String>,
}

impl fmt::Debug for ResolvedGateway {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedGateway")
            .field("gateway", &self.gateway.id)
            .field("protocol", &self.protocol)
            .field("endpoint", &self.endpoint)
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "[REDACTED]"),
            )
            .field("model", &self.model)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) enum GatewayResolutionError {
    NoDefaultGateway,
    GatewayNotFound,
    InvalidGateway(Vec<crate::gateway::ValidationError>),
    UnsupportedProtocol(GatewayProtocol),
    MissingCredential,
    InvalidCredential,
    UnexpectedCredential,
    CredentialStoreUnavailable,
    InvalidModel,
}

impl fmt::Display for GatewayResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDefaultGateway => formatter.write_str("no gateway is selected"),
            Self::GatewayNotFound => formatter.write_str("the selected gateway does not exist"),
            Self::InvalidGateway(errors) => {
                formatter.write_str("the selected gateway is invalid")?;
                for error in errors {
                    write!(formatter, "; {error}")?;
                }
                Ok(())
            }
            Self::UnsupportedProtocol(protocol) => write!(
                formatter,
                "the selected gateway does not support {}",
                protocol.display_name()
            ),
            Self::MissingCredential => {
                formatter.write_str("the selected gateway has no credential configured")
            }
            Self::InvalidCredential => formatter.write_str("the gateway credential is invalid"),
            Self::UnexpectedCredential => formatter.write_str(
                "GOWILD_API_KEY is set but the selected gateway is configured without authentication",
            ),
            Self::CredentialStoreUnavailable => {
                formatter.write_str("the gateway credential store is unavailable")
            }
            Self::InvalidModel => formatter.write_str("the selected model is invalid"),
        }
    }
}

impl std::error::Error for GatewayResolutionError {}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn invalid_model(model: &str) -> bool {
    model.trim() != model || model.len() > 256 || model.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::gateway::{CredentialBackend, CredentialStoreError};

    #[derive(Default)]
    struct MapEnvironment(BTreeMap<String, String>);

    impl Environment for MapEnvironment {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    #[derive(Default)]
    struct MemoryCredentials(BTreeMap<String, Credential>);

    impl CredentialStore for MemoryCredentials {
        fn get(&self, credential_ref: &str) -> Result<Option<Credential>, CredentialStoreError> {
            Ok(self.0.get(credential_ref).cloned())
        }

        fn set(
            &self,
            _credential_ref: &str,
            _credential: &Credential,
        ) -> Result<CredentialBackend, CredentialStoreError> {
            unreachable!()
        }

        fn delete(&self, _credential_ref: &str) -> Result<(), CredentialStoreError> {
            unreachable!()
        }
    }

    fn configured_catalog() -> GatewayCatalog {
        let mut catalog = GatewayCatalog::with_builtin_presets();
        catalog.default_gateway_id = Some("mindshub".into());
        catalog
            .gateways
            .get_mut("mindshub")
            .unwrap()
            .default_models
            .insert("codex".into(), "saved-model".into());
        catalog
    }

    fn stored_credentials() -> MemoryCredentials {
        MemoryCredentials(BTreeMap::from([(
            "gateway:mindshub".into(),
            Credential::new("saved-custom-key").unwrap(),
        )]))
    }

    #[test]
    fn explicit_selection_beats_environment_which_beats_saved_defaults() {
        let catalog = configured_catalog();
        let credentials = stored_credentials();
        let environment = MapEnvironment(BTreeMap::from([
            (ENV_GATEWAY.into(), "mindshub".into()),
            (ENV_MODEL.into(), "environment-model".into()),
            (
                ENV_RESPONSES_BASE_URL.into(),
                "https://override.example/v1".into(),
            ),
            (ENV_API_KEY.into(), "environment-custom-key".into()),
        ]));
        let resolver = GatewayResolver::new(&catalog, &credentials, &environment);

        let resolved = resolver
            .resolve(
                "codex",
                GatewayProtocol::OpenAiResponses,
                Some("mindshub"),
                Some("explicit-model"),
            )
            .unwrap();
        assert_eq!(resolved.model.as_deref(), Some("explicit-model"));
        assert_eq!(resolved.endpoint, "https://override.example/v1");
        assert_eq!(
            resolved.credential.as_ref().unwrap().expose(),
            "environment-custom-key"
        );
        assert!(!format!("{resolved:?}").contains("environment-custom-key"));
    }

    #[test]
    fn saved_model_and_credential_are_used_without_overrides() {
        let catalog = configured_catalog();
        let credentials = stored_credentials();
        let environment = MapEnvironment::default();
        let resolver = GatewayResolver::new(&catalog, &credentials, &environment);
        let resolved = resolver
            .resolve("codex", GatewayProtocol::OpenAiResponses, None, None)
            .unwrap();
        assert_eq!(resolved.model.as_deref(), Some("saved-model"));
        assert_eq!(
            resolved.credential.as_ref().unwrap().expose(),
            "saved-custom-key"
        );
    }

    #[test]
    fn incompatible_protocol_fails_before_launch() {
        let mut catalog = configured_catalog();
        let gateway = catalog.gateways.get_mut("mindshub").unwrap();
        gateway
            .capabilities
            .protocols
            .remove(&GatewayProtocol::AnthropicMessages);
        gateway.endpoints.anthropic_messages = None;
        let credentials = stored_credentials();
        let environment = MapEnvironment::default();
        let resolver = GatewayResolver::new(&catalog, &credentials, &environment);
        assert!(matches!(
            resolver.resolve("claude", GatewayProtocol::AnthropicMessages, None, None),
            Err(GatewayResolutionError::UnsupportedProtocol(
                GatewayProtocol::AnthropicMessages
            ))
        ));
    }
}
