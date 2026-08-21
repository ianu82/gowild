use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;

use super::credentials::Credential;
use super::redact::redact;

pub(crate) const CURRENT_SCHEMA_VERSION: u32 = 1;
pub(crate) const MINDSHUB_RESPONSES_BASE_URL: &str = "https://api.mindshub.ai/v1";
pub(crate) const MINDSHUB_ANTHROPIC_BASE_URL: &str = "https://api.mindshub.ai";
pub(crate) const MINDSHUB_MODELS_URL: &str = "https://api.mindshub.ai/v1/models";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayProtocol {
    OpenAiResponses,
    AnthropicMessages,
}

impl GatewayProtocol {
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "OpenAI Responses",
            Self::AnthropicMessages => "Anthropic Messages",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayFeature {
    ModelDiscovery,
    Streaming,
    ToolCalling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayCapabilities {
    #[serde(default)]
    pub(crate) protocols: BTreeSet<GatewayProtocol>,
    #[serde(default)]
    pub(crate) features: BTreeSet<GatewayFeature>,
}

impl GatewayCapabilities {
    pub(crate) fn supports(&self, protocol: GatewayProtocol) -> bool {
        self.protocols.contains(&protocol)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayPreset {
    MindsHubInference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuthenticationMode {
    #[default]
    BearerToken,
    XApiKey,
    CustomHeader,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayAuth {
    pub(crate) mode: AuthenticationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) credential_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) header_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) value_prefix: Option<String>,
}

impl GatewayAuth {
    pub(crate) fn bearer(credential_ref: impl Into<String>) -> Self {
        Self {
            mode: AuthenticationMode::BearerToken,
            credential_ref: Some(credential_ref.into()),
            header_name: None,
            value_prefix: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayEndpoints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) openai_responses: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) anthropic_messages: Option<String>,
}

impl GatewayEndpoints {
    pub(crate) fn for_protocol(&self, protocol: GatewayProtocol) -> Option<&str> {
        match protocol {
            GatewayProtocol::OpenAiResponses => self.openai_responses.as_deref(),
            GatewayProtocol::AnthropicMessages => self.anthropic_messages.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CachedModel {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider: Option<String>,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) embedding: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) reasoning_efforts: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelDiscovery {
    pub(crate) enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) cached_models: Vec<CachedModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) refreshed_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionStatus {
    #[default]
    NotTested,
    Passed,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DiagnosticLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Diagnostic {
    pub(crate) level: DiagnosticLevel,
    pub(crate) code: String,
    message: String,
}

impl Diagnostic {
    pub(crate) fn sanitized(
        level: DiagnosticLevel,
        code: impl Into<String>,
        message: &str,
        credentials: &[&Credential],
    ) -> Self {
        Self {
            level,
            code: code.into(),
            message: redact(message, credentials),
        }
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProtocolTest {
    pub(crate) status: ConnectionStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectionTest {
    pub(crate) status: ConnectionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) checked_at: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) protocols: BTreeMap<GatewayProtocol, ProtocolTest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Gateway {
    pub(crate) id: String,
    pub(crate) display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) preset: Option<GatewayPreset>,
    pub(crate) endpoints: GatewayEndpoints,
    pub(crate) capabilities: GatewayCapabilities,
    pub(crate) auth: GatewayAuth,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) custom_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) model_discovery: ModelDiscovery,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) default_models: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) connection_test: ConnectionTest,
}

impl Gateway {
    pub(crate) fn mindshub() -> Self {
        Self {
            id: "mindshub".into(),
            display_name: "MindsHub Inference".into(),
            preset: Some(GatewayPreset::MindsHubInference),
            endpoints: GatewayEndpoints {
                openai_responses: Some(MINDSHUB_RESPONSES_BASE_URL.into()),
                anthropic_messages: Some(MINDSHUB_ANTHROPIC_BASE_URL.into()),
            },
            capabilities: GatewayCapabilities {
                protocols: BTreeSet::from([
                    GatewayProtocol::OpenAiResponses,
                    GatewayProtocol::AnthropicMessages,
                ]),
                features: BTreeSet::from([
                    GatewayFeature::ModelDiscovery,
                    GatewayFeature::Streaming,
                    GatewayFeature::ToolCalling,
                ]),
            },
            auth: GatewayAuth::bearer("gateway:mindshub"),
            custom_headers: BTreeMap::new(),
            model_discovery: ModelDiscovery {
                enabled: true,
                url: Some(MINDSHUB_MODELS_URL.into()),
                ..ModelDiscovery::default()
            },
            default_models: BTreeMap::new(),
            connection_test: ConnectionTest::default(),
        }
    }

    pub(crate) fn supports(&self, protocol: GatewayProtocol) -> bool {
        self.capabilities.supports(protocol) && self.endpoints.for_protocol(protocol).is_some()
    }

    pub(crate) fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        validate_identifier("id", &self.id, &mut errors);
        validate_text("display_name", &self.display_name, 80, &mut errors);
        if self.capabilities.protocols.is_empty() {
            errors.push(ValidationError::new(
                "capabilities.protocols",
                "at least one supported protocol is required",
            ));
        }

        for (protocol, field) in [
            (
                GatewayProtocol::OpenAiResponses,
                "endpoints.openai_responses",
            ),
            (
                GatewayProtocol::AnthropicMessages,
                "endpoints.anthropic_messages",
            ),
        ] {
            match (
                self.capabilities.supports(protocol),
                self.endpoints.for_protocol(protocol),
            ) {
                (true, None) => errors.push(ValidationError::new(
                    field,
                    format!("{} support requires an endpoint", protocol.display_name()),
                )),
                (false, Some(_)) => errors.push(ValidationError::new(
                    field,
                    format!(
                        "an endpoint requires {} to be listed as supported",
                        protocol.display_name()
                    ),
                )),
                _ => {}
            }
        }

        if let Some(endpoint) = &self.endpoints.openai_responses {
            validate_url("endpoints.openai_responses", endpoint, &mut errors);
        }
        if let Some(endpoint) = &self.endpoints.anthropic_messages {
            validate_url("endpoints.anthropic_messages", endpoint, &mut errors);
        }

        match self.auth.mode {
            AuthenticationMode::None => {
                if self.auth.credential_ref.is_some() {
                    errors.push(ValidationError::new(
                        "auth.credential_ref",
                        "unauthenticated gateways cannot reference a credential",
                    ));
                }
                if self.auth.header_name.is_some() || self.auth.value_prefix.is_some() {
                    errors.push(ValidationError::new(
                        "auth",
                        "unauthenticated gateways cannot configure a secret-bearing header",
                    ));
                }
            }
            AuthenticationMode::BearerToken | AuthenticationMode::XApiKey => {
                validate_credential_ref(&self.auth.credential_ref, &mut errors);
                if self.auth.header_name.is_some() || self.auth.value_prefix.is_some() {
                    errors.push(ValidationError::new(
                        "auth",
                        "header_name and value_prefix are only valid for custom_header auth",
                    ));
                }
            }
            AuthenticationMode::CustomHeader => {
                validate_credential_ref(&self.auth.credential_ref, &mut errors);
                match self.auth.header_name.as_deref() {
                    Some(name) if valid_header_name(name) && !is_transport_controlled_header(name) => {
                        if self
                            .custom_headers
                            .keys()
                            .any(|custom| custom.eq_ignore_ascii_case(name))
                        {
                            errors.push(ValidationError::new(
                                "custom_headers",
                                "cannot override the configured authentication header",
                            ));
                        }
                    }
                    _ => errors.push(ValidationError::new(
                        "auth.header_name",
                        "custom header authentication requires a valid non-transport-controlled HTTP header name",
                    )),
                }
                if self
                    .auth
                    .value_prefix
                    .as_deref()
                    .is_some_and(contains_unsafe_header_value)
                {
                    errors.push(ValidationError::new(
                        "auth.value_prefix",
                        "header value prefix cannot contain control characters",
                    ));
                }
            }
        }

        if self.custom_headers.len() > 32 {
            errors.push(ValidationError::new(
                "custom_headers",
                "cannot contain more than 32 headers",
            ));
        }
        for (name, value) in &self.custom_headers {
            if !valid_header_name(name) {
                errors.push(ValidationError::new(
                    "custom_headers",
                    "contains an invalid HTTP header name",
                ));
            }
            if is_sensitive_header(name) {
                errors.push(ValidationError::new(
                    "custom_headers",
                    "a credential-bearing header must be configured through authentication instead",
                ));
            }
            if is_transport_controlled_header(name) {
                errors.push(ValidationError::new(
                    "custom_headers",
                    "transport-controlled HTTP headers cannot be overridden",
                ));
            }
            if value.len() > 4_096 || contains_unsafe_header_value(value) {
                errors.push(ValidationError::new(
                    "custom_headers",
                    "a header value is too long or contains control characters",
                ));
            }
        }

        if self.model_discovery.enabled {
            if !self
                .capabilities
                .features
                .contains(&GatewayFeature::ModelDiscovery)
            {
                errors.push(ValidationError::new(
                    "model_discovery.enabled",
                    "enabled model discovery must be listed as a gateway capability",
                ));
            }
            match self.model_discovery.url.as_deref() {
                Some(url) => validate_url("model_discovery.url", url, &mut errors),
                None => errors.push(ValidationError::new(
                    "model_discovery.url",
                    "model discovery is enabled but no URL is configured",
                )),
            }
        }
        if self.model_discovery.cached_models.len() > 10_000 {
            errors.push(ValidationError::new(
                "model_discovery.cached_models",
                "cannot contain more than 10,000 models",
            ));
        }
        for model in &self.model_discovery.cached_models {
            validate_text(
                "model_discovery.cached_models.id",
                &model.id,
                256,
                &mut errors,
            );
            for (field, value) in [
                ("model_discovery.cached_models.label", &model.label),
                ("model_discovery.cached_models.provider", &model.provider),
            ] {
                if value
                    .as_deref()
                    .is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control))
                {
                    errors.push(ValidationError::new(
                        field,
                        "is too long or contains control characters",
                    ));
                }
            }
            if model.reasoning_efforts.len() > 16
                || model.reasoning_efforts.iter().any(|effort| {
                    effort.is_empty() || effort.len() > 64 || effort.chars().any(char::is_control)
                })
            {
                errors.push(ValidationError::new(
                    "model_discovery.cached_models.reasoning_efforts",
                    "contains too many or invalid reasoning-effort labels",
                ));
            }
        }

        for (cli, model) in &self.default_models {
            validate_identifier("default_models", cli, &mut errors);
            validate_text("default_models", model, 256, &mut errors);
        }
        errors
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GatewayCatalog {
    pub(crate) schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_gateway_id: Option<String>,
    #[serde(default)]
    pub(crate) gateways: BTreeMap<String, Gateway>,
}

impl Default for GatewayCatalog {
    fn default() -> Self {
        Self::with_builtin_presets()
    }
}

impl GatewayCatalog {
    pub(crate) fn empty() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            default_gateway_id: None,
            gateways: BTreeMap::new(),
        }
    }

    pub(crate) fn with_builtin_presets() -> Self {
        let mut catalog = Self::empty();
        let mindshub = Gateway::mindshub();
        catalog.gateways.insert(mindshub.id.clone(), mindshub);
        catalog
    }

    pub(crate) fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            errors.push(ValidationError::new(
                "schema_version",
                format!(
                    "unsupported gateway schema version {}; expected {}",
                    self.schema_version, CURRENT_SCHEMA_VERSION
                ),
            ));
        }
        if let Some(default_id) = &self.default_gateway_id {
            if !self.gateways.contains_key(default_id) {
                errors.push(ValidationError::new(
                    "default_gateway_id",
                    "the selected default gateway does not exist",
                ));
            }
        }
        if self.gateways.len() > 100 {
            errors.push(ValidationError::new(
                "gateways",
                "cannot contain more than 100 gateways",
            ));
        }
        for (key, gateway) in &self.gateways {
            if !identifier_is_valid(key) {
                errors.push(ValidationError::new(
                    "gateways",
                    "a gateway map key is not a valid gateway id",
                ));
            }
            if key != &gateway.id {
                errors.push(ValidationError::new(
                    "gateways",
                    "a gateway map key does not match its gateway id",
                ));
            }
            let gateway_field = if identifier_is_valid(&gateway.id) {
                format!("gateways.{}", gateway.id)
            } else {
                "gateways.<invalid>".into()
            };
            errors.extend(gateway.validate().into_iter().map(|error| {
                ValidationError::new(format!("{gateway_field}.{}", error.field), error.message)
            }));
        }
        errors
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidationError {
    pub(crate) field: String,
    pub(crate) message: String,
}

impl ValidationError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

fn validate_identifier(field: &str, value: &str, errors: &mut Vec<ValidationError>) {
    if !identifier_is_valid(value) {
        errors.push(ValidationError::new(
            field,
            "must be 1-64 lowercase letters, numbers, or hyphens and start with a letter or number",
        ));
    }
}

fn validate_text(field: &str, value: &str, max_len: usize, errors: &mut Vec<ValidationError>) {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > max_len
        || value.chars().any(char::is_control)
    {
        errors.push(ValidationError::new(
            field,
            "is required, cannot have surrounding whitespace, and must contain safe text within the length limit",
        ));
    }
}

fn identifier_is_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn validate_credential_ref(value: &Option<String>, errors: &mut Vec<ValidationError>) {
    match value.as_deref() {
        Some(value)
            if !value.trim().is_empty()
                && value.trim() == value
                && value.len() <= 128
                && !value.chars().any(char::is_control) => {}
        _ => errors.push(ValidationError::new(
            "auth.credential_ref",
            "authenticated gateways require a non-secret credential reference",
        )),
    }
}

fn validate_url(field: &str, value: &str, errors: &mut Vec<ValidationError>) {
    if value.trim() != value {
        errors.push(ValidationError::new(
            field,
            "must not have leading or trailing whitespace",
        ));
        return;
    }
    let parsed = match Url::parse(value) {
        Ok(parsed) => parsed,
        Err(_) => {
            errors.push(ValidationError::new(field, "must be a valid URL"));
            return;
        }
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        errors.push(ValidationError::new(field, "must use http or https"));
    }
    if parsed.scheme() == "http"
        && !parsed
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
        && !parsed.host().is_some_and(|host| match host {
            url::Host::Ipv4(address) => address.is_loopback(),
            url::Host::Ipv6(address) => address.is_loopback(),
            url::Host::Domain(_) => false,
        })
    {
        errors.push(ValidationError::new(
            field,
            "must use https unless the gateway is on the loopback interface",
        ));
    }
    if parsed.host_str().is_none() {
        errors.push(ValidationError::new(field, "must include a host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        errors.push(ValidationError::new(
            field,
            "must not embed credentials in the URL",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        errors.push(ValidationError::new(
            field,
            "must not include a query string or fragment",
        ));
    }
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "proxy-authorization" | "x-api-key" | "api-key" | "apikey"
    )
}

fn is_transport_controlled_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "content-length"
            | "host"
            | "keep-alive"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn contains_unsafe_header_value(value: &str) -> bool {
    value.chars().any(|character| character.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mindshub_preset_matches_documented_protocol_endpoints() {
        let gateway = Gateway::mindshub();
        assert_eq!(
            gateway.endpoints.openai_responses.as_deref(),
            Some("https://api.mindshub.ai/v1")
        );
        assert_eq!(
            gateway.endpoints.anthropic_messages.as_deref(),
            Some("https://api.mindshub.ai")
        );
        assert_eq!(
            gateway.model_discovery.url.as_deref(),
            Some("https://api.mindshub.ai/v1/models")
        );
        assert_eq!(gateway.auth.mode, AuthenticationMode::BearerToken);
        assert!(gateway.supports(GatewayProtocol::OpenAiResponses));
        assert!(gateway.supports(GatewayProtocol::AnthropicMessages));
        assert!(gateway.validate().is_empty());
    }

    #[test]
    fn protocol_support_requires_matching_endpoint() {
        let mut gateway = Gateway::mindshub();
        gateway.endpoints.anthropic_messages = None;
        let errors = gateway.validate();
        assert!(errors.iter().any(|error| {
            error.field == "endpoints.anthropic_messages"
                && error.message.contains("requires an endpoint")
        }));
        assert!(!gateway.supports(GatewayProtocol::AnthropicMessages));
    }

    #[test]
    fn endpoint_requires_matching_protocol_capability() {
        let mut gateway = Gateway::mindshub();
        gateway
            .capabilities
            .protocols
            .remove(&GatewayProtocol::AnthropicMessages);
        let errors = gateway.validate();
        assert!(errors.iter().any(|error| {
            error.field == "endpoints.anthropic_messages"
                && error.message.contains("listed as supported")
        }));
        assert!(!gateway.supports(GatewayProtocol::AnthropicMessages));
    }

    #[test]
    fn validation_rejects_urls_with_embedded_credentials() {
        let mut gateway = Gateway::mindshub();
        gateway.endpoints.openai_responses = Some("https://user:secret@example.com/v1".into());
        let errors = gateway.validate();
        assert!(errors.iter().any(|error| {
            error.field == "endpoints.openai_responses"
                && error.message.contains("must not embed credentials")
        }));
    }

    #[test]
    fn remote_http_endpoints_are_rejected_but_loopback_is_allowed() {
        let mut gateway = Gateway::mindshub();
        gateway.endpoints.openai_responses = Some("http://gateway.example/v1".into());
        assert!(gateway.validate().iter().any(|error| {
            error.field == "endpoints.openai_responses" && error.message.contains("must use https")
        }));

        gateway.endpoints.openai_responses = Some("http://127.0.0.1:8080/v1".into());
        gateway.endpoints.anthropic_messages = Some("http://[::1]:8080".into());
        gateway.model_discovery.url = Some("http://localhost:8080/v1/models".into());
        assert!(gateway.validate().is_empty());
    }

    #[test]
    fn validation_rejects_endpoint_whitespace_instead_of_silently_trimming() {
        let mut gateway = Gateway::mindshub();
        gateway.endpoints.openai_responses = Some(" https://api.mindshub.ai/v1 ".into());
        let errors = gateway.validate();
        assert!(errors.iter().any(|error| {
            error.field == "endpoints.openai_responses"
                && error.message.contains("leading or trailing whitespace")
        }));
    }

    #[test]
    fn validation_keeps_sensitive_headers_out_of_non_secret_config() {
        let mut gateway = Gateway::mindshub();
        gateway
            .custom_headers
            .insert("Authorization".into(), "Bearer should-not-be-here".into());
        let errors = gateway.validate();
        assert!(errors
            .iter()
            .any(|error| error.message.contains("configured through authentication")));
    }

    #[test]
    fn validation_rejects_a_custom_header_that_overrides_authentication() {
        let mut gateway = Gateway::mindshub();
        gateway.auth.mode = AuthenticationMode::CustomHeader;
        gateway.auth.header_name = Some("X-Gateway-Token".into());
        gateway.auth.value_prefix = Some("Token ".into());
        gateway
            .custom_headers
            .insert("x-gateway-token".into(), "non-secret".into());

        assert!(gateway.validate().iter().any(|error| {
            error.field == "custom_headers" && error.message.contains("authentication header")
        }));
    }

    #[test]
    fn validation_rejects_transport_controlled_custom_headers() {
        let mut gateway = Gateway::mindshub();
        gateway
            .custom_headers
            .insert("Content-Length".into(), "4".into());
        assert!(gateway.validate().iter().any(|error| {
            error.field == "custom_headers" && error.message.contains("transport-controlled")
        }));

        gateway.custom_headers.clear();
        gateway.auth.mode = AuthenticationMode::CustomHeader;
        gateway.auth.header_name = Some("Host".into());
        assert!(gateway.validate().iter().any(|error| {
            error.field == "auth.header_name" && error.message.contains("transport-controlled")
        }));
    }

    #[test]
    fn diagnostics_defensively_redact_common_key_shapes() {
        let diagnostic = Diagnostic::sanitized(
            DiagnosticLevel::Error,
            "auth_failed",
            "server rejected Authorization: Bearer mdb_supersecret123",
            &[],
        );
        assert!(!diagnostic.message().contains("mdb_supersecret123"));
        assert!(diagnostic.message().contains("[REDACTED]"));
    }

    #[test]
    fn diagnostics_redact_the_exact_custom_credential() {
        let credential = Credential::new("custom key with an unusual shape").unwrap();
        let diagnostic = Diagnostic::sanitized(
            DiagnosticLevel::Error,
            "auth_failed",
            "gateway echoed custom key with an unusual shape",
            &[&credential],
        );
        assert_eq!(diagnostic.message(), "gateway echoed [REDACTED]");
    }

    #[test]
    fn catalog_rejects_a_missing_default() {
        let mut catalog = GatewayCatalog::with_builtin_presets();
        catalog.default_gateway_id = Some("missing".into());
        assert!(catalog
            .validate()
            .iter()
            .any(|error| error.field == "default_gateway_id"));
    }
}
