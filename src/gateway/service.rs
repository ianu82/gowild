use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Read;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::model::{
    AuthenticationMode, CachedModel, ConnectionStatus, ConnectionTest, Diagnostic, DiagnosticLevel,
    Gateway, GatewayProtocol, ProtocolTest,
};
use super::Credential;

const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MODELS: usize = 10_000;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

pub(crate) struct GatewayTester {
    client: Client,
}

impl GatewayTester {
    pub(crate) fn new() -> Result<Self, GatewayTesterError> {
        Self::from_builder(Client::builder())
    }

    fn from_builder(builder: reqwest::blocking::ClientBuilder) -> Result<Self, GatewayTesterError> {
        let client = builder
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .timeout(DEFAULT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("gowild/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| GatewayTesterError::ClientUnavailable)?;
        Ok(Self { client })
    }

    pub(crate) fn inspect(
        &self,
        gateway: &Gateway,
        credential: Option<&Credential>,
    ) -> GatewayInspection {
        let checked_at = timestamp();
        let credential_list = credential.into_iter().collect::<Vec<_>>();
        let mut diagnostics = Vec::new();
        let mut protocols = BTreeMap::new();
        let mut successes = 0usize;
        let mut failures = 0usize;

        let validation = gateway.validate();
        if !validation.is_empty() {
            let message = format!(
                "Gateway configuration is invalid: {}",
                validation
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            diagnostics.push(Diagnostic::sanitized(
                DiagnosticLevel::Error,
                "invalid_gateway",
                &message,
                &credential_list,
            ));
            return GatewayInspection {
                discovered_models: None,
                connection_test: ConnectionTest {
                    status: ConnectionStatus::Failed,
                    checked_at: Some(checked_at),
                    protocols,
                    diagnostics,
                },
            };
        }

        if gateway.auth.mode != AuthenticationMode::None && credential.is_none() {
            diagnostics.push(Diagnostic::sanitized(
                DiagnosticLevel::Error,
                "missing_credential",
                "The gateway credential is missing.",
                &credential_list,
            ));
            return GatewayInspection {
                discovered_models: None,
                connection_test: ConnectionTest {
                    status: ConnectionStatus::Failed,
                    checked_at: Some(checked_at),
                    protocols,
                    diagnostics,
                },
            };
        }

        let discovered_models = if gateway.model_discovery.enabled {
            match self.discover_models(gateway, credential) {
                Ok(discovery) => {
                    successes += 1;
                    diagnostics.push(Diagnostic::sanitized(
                        DiagnosticLevel::Info,
                        "authentication_passed",
                        "Gateway authentication was accepted.",
                        &credential_list,
                    ));
                    let message = format!("Discovered {} models.", discovery.models.len());
                    diagnostics.push(Diagnostic::sanitized(
                        DiagnosticLevel::Info,
                        "model_discovery_passed",
                        &message,
                        &credential_list,
                    ));
                    if discovery.omitted > 0 {
                        let message = format!(
                            "Ignored {} malformed or duplicate model entries.",
                            discovery.omitted
                        );
                        diagnostics.push(Diagnostic::sanitized(
                            DiagnosticLevel::Warning,
                            "model_entries_ignored",
                            &message,
                            &credential_list,
                        ));
                    }
                    Some(discovery.models)
                }
                Err(error) => {
                    diagnostics.push(error.diagnostic(&credential_list));
                    for protocol in &gateway.capabilities.protocols {
                        protocols.insert(
                            *protocol,
                            ProtocolTest {
                                status: ConnectionStatus::Failed,
                                diagnostics: vec![Diagnostic::sanitized(
                                    DiagnosticLevel::Error,
                                    "protocol_not_tested",
                                    "The protocol test was skipped because authenticated model discovery failed.",
                                    &credential_list,
                                )],
                            },
                        );
                    }
                    return GatewayInspection {
                        discovered_models: None,
                        connection_test: ConnectionTest {
                            status: ConnectionStatus::Failed,
                            checked_at: Some(checked_at),
                            protocols,
                            diagnostics,
                        },
                    };
                }
            }
        } else {
            None
        };

        let catalog_is_authoritative = discovered_models.is_some();
        let available_models = discovered_models
            .as_deref()
            .unwrap_or(&gateway.model_discovery.cached_models);
        for protocol in &gateway.capabilities.protocols {
            let result = match select_probe_model(
                gateway,
                *protocol,
                available_models,
                catalog_is_authoritative,
            ) {
                Some(model) => self.probe_protocol(gateway, credential, *protocol, model),
                None => Err(ProbeFailure::new(
                    "model_unavailable",
                    None,
                    "No enabled non-embedding model is available for this protocol test.",
                )),
            };
            match result {
                Ok(()) => {
                    successes += 1;
                    protocols.insert(
                        *protocol,
                        ProtocolTest {
                            status: ConnectionStatus::Passed,
                            diagnostics: vec![Diagnostic::sanitized(
                                DiagnosticLevel::Info,
                                protocol_success_code(*protocol),
                                protocol_success_message(*protocol),
                                &credential_list,
                            )],
                        },
                    );
                }
                Err(error) => {
                    failures += 1;
                    protocols.insert(
                        *protocol,
                        ProtocolTest {
                            status: ConnectionStatus::Failed,
                            diagnostics: vec![error.diagnostic(&credential_list)],
                        },
                    );
                }
            }
        }

        let status = match (successes, failures) {
            (0, _) => ConnectionStatus::Failed,
            (_, 0) => ConnectionStatus::Passed,
            _ => ConnectionStatus::Partial,
        };
        GatewayInspection {
            discovered_models,
            connection_test: ConnectionTest {
                status,
                checked_at: Some(checked_at),
                protocols,
                diagnostics,
            },
        }
    }

    fn discover_models(
        &self,
        gateway: &Gateway,
        credential: Option<&Credential>,
    ) -> Result<ModelDiscoveryResult, ProbeFailure> {
        let url = gateway.model_discovery.url.as_deref().ok_or_else(|| {
            ProbeFailure::new(
                "model_discovery_url_missing",
                None,
                "Model discovery is enabled but its URL is missing.",
            )
        })?;
        let response = self.send(gateway, credential, Method::GET, url, None)?;
        let payload: ModelsResponse = serde_json::from_slice(&response).map_err(|_| {
            ProbeFailure::new(
                "invalid_model_catalog",
                None,
                "The model discovery endpoint returned invalid JSON.",
            )
        })?;
        if payload.data.len() > MAX_MODELS {
            return Err(ProbeFailure::new(
                "model_catalog_too_large",
                None,
                "The model discovery endpoint returned too many models.",
            ));
        }

        let mut seen = BTreeSet::new();
        let mut models = Vec::new();
        let mut omitted = 0usize;
        for remote in payload.data {
            let valid_id = valid_model_text(&remote.id, 256);
            let unique = valid_id && seen.insert(remote.id.clone());
            if !unique {
                omitted += 1;
                continue;
            }
            let label = remote.label.filter(|value| valid_model_text(value, 256));
            let provider = remote.provider.filter(|value| valid_model_text(value, 256));
            let reasoning_efforts = remote
                .reasoning_efforts
                .unwrap_or_default()
                .into_iter()
                .filter(|value| valid_model_text(value, 64))
                .take(16)
                .collect();
            models.push(CachedModel {
                id: remote.id,
                label,
                provider,
                enabled: remote.enabled,
                embedding: remote.embedding,
                reasoning_efforts,
            });
        }
        Ok(ModelDiscoveryResult { models, omitted })
    }

    fn probe_protocol(
        &self,
        gateway: &Gateway,
        credential: Option<&Credential>,
        protocol: GatewayProtocol,
        model: &str,
    ) -> Result<(), ProbeFailure> {
        let (url, body) = match protocol {
            GatewayProtocol::OpenAiResponses => {
                let base = gateway
                    .endpoints
                    .openai_responses
                    .as_deref()
                    .ok_or_else(|| {
                        ProbeFailure::new(
                            "responses_endpoint_missing",
                            None,
                            "The Responses endpoint is missing.",
                        )
                    })?;
                (
                    append_path(base, "responses"),
                    json!({
                        "model": model,
                        "input": "Reply with OK.",
                        "max_output_tokens": 8,
                        "store": false
                    }),
                )
            }
            GatewayProtocol::AnthropicMessages => {
                let base = gateway
                    .endpoints
                    .anthropic_messages
                    .as_deref()
                    .ok_or_else(|| {
                        ProbeFailure::new(
                            "messages_endpoint_missing",
                            None,
                            "The Messages endpoint is missing.",
                        )
                    })?;
                (
                    append_path(base, "v1/messages"),
                    json!({
                        "model": model,
                        "max_tokens": 8,
                        "messages": [{"role": "user", "content": "Reply with OK."}]
                    }),
                )
            }
        };
        let response = self.send(gateway, credential, Method::POST, &url, Some(&body))?;
        let payload: Value = serde_json::from_slice(&response).map_err(|_| {
            ProbeFailure::new(
                protocol_invalid_response_code(protocol),
                None,
                "The protocol endpoint returned invalid JSON.",
            )
        })?;
        let compatible = match protocol {
            GatewayProtocol::OpenAiResponses => {
                payload.get("object").and_then(Value::as_str) == Some("response")
                    && payload.get("id").and_then(Value::as_str).is_some()
                    && payload.get("output").and_then(Value::as_array).is_some()
            }
            GatewayProtocol::AnthropicMessages => {
                payload.get("type").and_then(Value::as_str) == Some("message")
                    && payload.get("role").and_then(Value::as_str) == Some("assistant")
                    && payload.get("content").and_then(Value::as_array).is_some()
            }
        };
        if compatible {
            Ok(())
        } else {
            Err(ProbeFailure::new(
                protocol_invalid_response_code(protocol),
                None,
                "The endpoint returned JSON that does not match the required protocol shape.",
            ))
        }
    }

    fn send(
        &self,
        gateway: &Gateway,
        credential: Option<&Credential>,
        method: Method,
        url: &str,
        body: Option<&Value>,
    ) -> Result<Vec<u8>, ProbeFailure> {
        let headers = request_headers(gateway, credential)?;
        let mut request = self
            .client
            .request(method, url)
            .headers(headers)
            .header(ACCEPT, "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().map_err(|error| transport_failure(&error))?;
        read_response(response)
    }
}

#[derive(Debug)]
pub(crate) struct GatewayInspection {
    pub(crate) discovered_models: Option<Vec<CachedModel>>,
    pub(crate) connection_test: ConnectionTest,
}

impl GatewayInspection {
    pub(crate) fn apply_to(self, gateway: &mut Gateway) {
        if let Some(models) = self.discovered_models {
            gateway.model_discovery.cached_models = models;
            gateway.model_discovery.refreshed_at = self.connection_test.checked_at.clone();
        }
        gateway.connection_test = self.connection_test;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GatewayTesterError {
    ClientUnavailable,
}

impl fmt::Display for GatewayTesterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the gateway HTTP client could not be initialized")
    }
}

impl std::error::Error for GatewayTesterError {}

struct ModelDiscoveryResult {
    models: Vec<CachedModel>,
    omitted: usize,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<RemoteModel>,
}

#[derive(Deserialize)]
struct RemoteModel {
    id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    embedding: bool,
    #[serde(default)]
    reasoning_efforts: Option<Vec<String>>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug)]
struct ProbeFailure {
    code: &'static str,
    status: Option<StatusCode>,
    message: String,
}

impl ProbeFailure {
    fn new(code: &'static str, status: Option<StatusCode>, message: impl Into<String>) -> Self {
        Self {
            code,
            status,
            message: message.into(),
        }
    }

    fn diagnostic(&self, credentials: &[&Credential]) -> Diagnostic {
        let level = if self.status == Some(StatusCode::TOO_MANY_REQUESTS) {
            DiagnosticLevel::Warning
        } else {
            DiagnosticLevel::Error
        };
        Diagnostic::sanitized(level, self.code, &self.message, credentials)
    }
}

fn request_headers(
    gateway: &Gateway,
    credential: Option<&Credential>,
) -> Result<HeaderMap, ProbeFailure> {
    let mut headers = HeaderMap::new();
    for (name, value) in &gateway.custom_headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            ProbeFailure::new(
                "invalid_header",
                None,
                "The gateway contains an invalid custom header name.",
            )
        })?;
        let value = HeaderValue::from_str(value).map_err(|_| {
            ProbeFailure::new(
                "invalid_header",
                None,
                "The gateway contains an invalid custom header value.",
            )
        })?;
        headers.insert(name, value);
    }

    let Some((name, value)) = authentication_header(gateway, credential)? else {
        return Ok(headers);
    };
    let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
        ProbeFailure::new(
            "invalid_authentication_header",
            None,
            "The authentication header name is invalid.",
        )
    })?;
    let mut value = HeaderValue::from_str(&value).map_err(|_| {
        ProbeFailure::new(
            "invalid_authentication_header",
            None,
            "The authentication header value is invalid.",
        )
    })?;
    value.set_sensitive(true);
    headers.insert(name, value);
    Ok(headers)
}

fn authentication_header(
    gateway: &Gateway,
    credential: Option<&Credential>,
) -> Result<Option<(String, String)>, ProbeFailure> {
    match gateway.auth.mode {
        AuthenticationMode::None => Ok(None),
        AuthenticationMode::BearerToken => Ok(Some((
            AUTHORIZATION.as_str().into(),
            format!("Bearer {}", required_credential(credential)?.expose()),
        ))),
        AuthenticationMode::XApiKey => Ok(Some((
            "x-api-key".into(),
            required_credential(credential)?.expose().into(),
        ))),
        AuthenticationMode::CustomHeader => Ok(Some((
            gateway.auth.header_name.clone().ok_or_else(|| {
                ProbeFailure::new(
                    "invalid_authentication_header",
                    None,
                    "The authentication header name is missing.",
                )
            })?,
            format!(
                "{}{}",
                gateway.auth.value_prefix.as_deref().unwrap_or(""),
                required_credential(credential)?.expose()
            ),
        ))),
    }
}

fn required_credential(value: Option<&Credential>) -> Result<&Credential, ProbeFailure> {
    value.ok_or_else(|| {
        ProbeFailure::new(
            "missing_credential",
            None,
            "The gateway credential is missing.",
        )
    })
}

fn read_response(mut response: Response) -> Result<Vec<u8>, ProbeFailure> {
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            ProbeFailure::new(
                "response_read_failed",
                Some(status),
                "The gateway response could not be read.",
            )
        })?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(ProbeFailure::new(
            "response_too_large",
            Some(status),
            "The gateway response exceeded the safe size limit.",
        ));
    }
    if status.is_success() {
        return Ok(bytes);
    }

    let detail = response_error_message(&bytes);
    let (code, summary) = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => (
            "authentication_failed",
            "Gateway authentication was rejected.",
        ),
        StatusCode::PAYMENT_REQUIRED => (
            "payment_required",
            "The gateway rejected the request because billing or quota is unavailable.",
        ),
        StatusCode::TOO_MANY_REQUESTS => (
            "rate_limited",
            "The gateway rate-limited the connection test.",
        ),
        StatusCode::NOT_FOUND => (
            "endpoint_not_found",
            "The configured gateway endpoint was not found.",
        ),
        _ if status.is_server_error() => (
            "gateway_unavailable",
            "The gateway returned a server error.",
        ),
        _ => (
            "gateway_request_failed",
            "The gateway rejected the request.",
        ),
    };
    let message = detail.map_or_else(
        || format!("{summary} HTTP {}.", status.as_u16()),
        |detail| format!("{summary} HTTP {}: {detail}", status.as_u16()),
    );
    Err(ProbeFailure::new(code, Some(status), message))
}

fn response_error_message(bytes: &[u8]) -> Option<String> {
    let payload: Value = serde_json::from_slice(bytes).ok()?;
    let message = payload
        .pointer("/error/message")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)?;
    Some(message.chars().take(512).collect())
}

fn transport_failure(error: &reqwest::Error) -> ProbeFailure {
    let (code, message) = if error.is_timeout() {
        (
            "network_timeout",
            "The gateway connection timed out before a response arrived.",
        )
    } else if error.is_connect() {
        (
            "network_connect_failed",
            "GoWild could not connect to the configured gateway.",
        )
    } else if error.is_builder() {
        (
            "invalid_request",
            "GoWild could not build a valid gateway request.",
        )
    } else {
        (
            "network_failure",
            "The gateway request failed because of a network error.",
        )
    };
    ProbeFailure::new(code, None, message)
}

fn select_probe_model<'a>(
    gateway: &'a Gateway,
    protocol: GatewayProtocol,
    models: &'a [CachedModel],
    catalog_is_authoritative: bool,
) -> Option<&'a str> {
    let cli = match protocol {
        GatewayProtocol::OpenAiResponses => "codex",
        GatewayProtocol::AnthropicMessages => "claude",
    };
    if let Some(configured) = gateway.default_models.get(cli) {
        return match models.iter().find(|model| model.id == *configured) {
            Some(model) if model.enabled && !model.embedding => Some(configured),
            Some(_) => None,
            None if !catalog_is_authoritative => Some(configured),
            None => None,
        };
    }
    models
        .iter()
        .find(|model| model.enabled && !model.embedding)
        .map(|model| model.id.as_str())
}

fn append_path(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn valid_model_text(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= max
        && !value.chars().any(char::is_control)
}

fn timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

fn protocol_success_code(protocol: GatewayProtocol) -> &'static str {
    match protocol {
        GatewayProtocol::OpenAiResponses => "responses_passed",
        GatewayProtocol::AnthropicMessages => "messages_passed",
    }
}

fn protocol_success_message(protocol: GatewayProtocol) -> &'static str {
    match protocol {
        GatewayProtocol::OpenAiResponses => {
            "The endpoint returned a valid OpenAI Responses response."
        }
        GatewayProtocol::AnthropicMessages => {
            "The endpoint returned a valid Anthropic Messages response."
        }
    }
}

fn protocol_invalid_response_code(protocol: GatewayProtocol) -> &'static str {
    match protocol {
        GatewayProtocol::OpenAiResponses => "invalid_responses_shape",
        GatewayProtocol::AnthropicMessages => "invalid_messages_shape",
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;

    struct MockServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
        thread: thread::JoinHandle<()>,
    }

    impl MockServer {
        fn start(responses: Vec<(u16, &'static str)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&requests);
            let thread = thread::spawn(move || {
                for (status, body) in responses {
                    let (mut stream, _) = listener.accept().unwrap();
                    let request = read_request(&mut stream);
                    captured.lock().unwrap().push(request);
                    let reason = if status == 200 { "OK" } else { "Error" };
                    write!(
                        stream,
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                    stream.flush().unwrap();
                }
            });
            Self {
                base_url,
                requests,
                thread,
            }
        }

        fn finish(self) -> Vec<String> {
            self.thread.join().unwrap();
            Arc::try_unwrap(self.requests)
                .unwrap()
                .into_inner()
                .unwrap()
        }
    }

    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length: ")
                    .or_else(|| line.strip_prefix("Content-Length: "))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8(bytes).unwrap()
    }

    fn tester() -> GatewayTester {
        GatewayTester::from_builder(Client::builder().no_proxy()).unwrap()
    }

    fn local_gateway(base_url: &str) -> Gateway {
        let mut gateway = Gateway::mindshub();
        gateway.endpoints.openai_responses = Some(format!("{base_url}/v1"));
        gateway.endpoints.anthropic_messages = Some(base_url.into());
        gateway.model_discovery.url = Some(format!("{base_url}/v1/models"));
        gateway
            .default_models
            .insert("codex".into(), "sonnet".into());
        gateway
            .default_models
            .insert("claude".into(), "sonnet".into());
        gateway
    }

    #[test]
    fn authenticated_discovery_and_both_protocol_probes_use_real_http() {
        let server = MockServer::start(vec![
            (
                200,
                r#"{"object":"list","data":[{"id":"sonnet","label":"Claude Sonnet","enabled":true,"provider":"anthropic","reasoning_efforts":["low","high"]},{"id":"embed","label":"Embedding","enabled":true,"embedding":true}]}"#,
            ),
            (
                200,
                r#"{"id":"resp_1","object":"response","output":[],"status":"completed"}"#,
            ),
            (
                200,
                r#"{"id":"msg_1","type":"message","role":"assistant","content":[]}"#,
            ),
        ]);
        let mut gateway = local_gateway(&server.base_url);
        let credential = Credential::new("mdb_mock-secret-value").unwrap();
        let inspection = tester().inspect(&gateway, Some(&credential));

        assert_eq!(inspection.connection_test.status, ConnectionStatus::Passed);
        assert_eq!(inspection.discovered_models.as_ref().unwrap().len(), 2);
        inspection.apply_to(&mut gateway);
        assert_eq!(gateway.model_discovery.cached_models[0].id, "sonnet");
        assert!(gateway.model_discovery.cached_models[1].embedding);
        assert_eq!(
            gateway
                .connection_test
                .protocols
                .get(&GatewayProtocol::OpenAiResponses)
                .unwrap()
                .status,
            ConnectionStatus::Passed
        );
        assert_eq!(
            gateway
                .connection_test
                .protocols
                .get(&GatewayProtocol::AnthropicMessages)
                .unwrap()
                .status,
            ConnectionStatus::Passed
        );

        let requests = server.finish();
        assert_eq!(requests.len(), 3);
        assert!(requests[0].starts_with("GET /v1/models "));
        assert!(requests[1].starts_with("POST /v1/responses "));
        assert!(requests[2].starts_with("POST /v1/messages "));
        for request in &requests {
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer mdb_mock-secret-value"));
        }
        assert!(requests[1].contains(r#""store":false"#));
        assert!(requests[2].contains(r#""max_tokens":8"#));
    }

    #[test]
    fn authentication_failures_are_redacted_and_stop_protocol_probes() {
        let secret = "mdb_secret-never-persist";
        let error = format!(r#"{{"error":{{"message":"credential {secret} was rejected"}}}}"#);
        let error: &'static str = Box::leak(error.into_boxed_str());
        let server = MockServer::start(vec![(401, error)]);
        let gateway = local_gateway(&server.base_url);
        let credential = Credential::new(secret).unwrap();
        let inspection = tester().inspect(&gateway, Some(&credential));

        assert_eq!(inspection.connection_test.status, ConnectionStatus::Failed);
        assert!(inspection.connection_test.protocols.values().all(|test| {
            test.status == ConnectionStatus::Failed
                && test.diagnostics.iter().all(|diagnostic| {
                    !diagnostic.message().contains(secret)
                        && !format!("{diagnostic:?}").contains(secret)
                })
        }));
        assert!(inspection
            .connection_test
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message().contains(secret)));
        assert_eq!(server.finish().len(), 1);
    }

    #[test]
    fn all_authentication_modes_build_the_expected_sensitive_header() {
        let credential = Credential::new("mock-secret").unwrap();
        let mut gateway = Gateway::mindshub();

        let bearer = request_headers(&gateway, Some(&credential)).unwrap();
        assert_eq!(bearer[AUTHORIZATION], "Bearer mock-secret");
        assert!(bearer[AUTHORIZATION].is_sensitive());

        gateway.auth.mode = AuthenticationMode::XApiKey;
        let x_api_key = request_headers(&gateway, Some(&credential)).unwrap();
        assert_eq!(x_api_key["x-api-key"], "mock-secret");
        assert!(x_api_key["x-api-key"].is_sensitive());

        gateway.auth.mode = AuthenticationMode::CustomHeader;
        gateway.auth.header_name = Some("X-Gateway-Token".into());
        gateway.auth.value_prefix = Some("Token ".into());
        let custom = request_headers(&gateway, Some(&credential)).unwrap();
        assert_eq!(custom["x-gateway-token"], "Token mock-secret");
        assert!(custom["x-gateway-token"].is_sensitive());

        gateway.auth.mode = AuthenticationMode::None;
        gateway.auth.credential_ref = None;
        gateway.auth.header_name = None;
        gateway.auth.value_prefix = None;
        assert!(request_headers(&gateway, None).unwrap().is_empty());
    }

    #[test]
    fn disabled_or_embedding_defaults_fail_before_a_generation_request() {
        let server = MockServer::start(vec![(
            200,
            r#"{"data":[{"id":"sonnet","enabled":false},{"id":"embed","enabled":true,"embedding":true}]}"#,
        )]);
        let gateway = local_gateway(&server.base_url);
        let credential = Credential::new("mock-secret").unwrap();
        let inspection = tester().inspect(&gateway, Some(&credential));

        assert_eq!(inspection.connection_test.status, ConnectionStatus::Partial);
        assert!(inspection
            .connection_test
            .protocols
            .values()
            .all(|test| test.status == ConnectionStatus::Failed));
        assert_eq!(server.finish().len(), 1);
    }

    #[test]
    fn an_authoritative_empty_catalog_does_not_fall_back_to_configured_models() {
        let server = MockServer::start(vec![(200, r#"{"object":"list","data":[]}"#)]);
        let gateway = local_gateway(&server.base_url);
        let credential = Credential::new("mock-secret").unwrap();
        let inspection = tester().inspect(&gateway, Some(&credential));

        assert_eq!(inspection.connection_test.status, ConnectionStatus::Partial);
        assert!(inspection
            .connection_test
            .protocols
            .values()
            .all(|test| test.status == ConnectionStatus::Failed));
        assert_eq!(server.finish().len(), 1);
    }

    #[test]
    fn a_stale_cache_does_not_block_an_unlisted_model_when_discovery_is_disabled() {
        let server = MockServer::start(vec![
            (200, r#"{"id":"resp_1","object":"response","output":[]}"#),
            (
                200,
                r#"{"id":"msg_1","type":"message","role":"assistant","content":[]}"#,
            ),
        ]);
        let mut gateway = local_gateway(&server.base_url);
        gateway.model_discovery.enabled = false;
        gateway.model_discovery.cached_models = vec![CachedModel {
            id: "old-model".into(),
            label: None,
            provider: None,
            enabled: true,
            embedding: false,
            reasoning_efforts: Vec::new(),
        }];
        let credential = Credential::new("mock-secret").unwrap();
        let inspection = tester().inspect(&gateway, Some(&credential));

        assert_eq!(inspection.connection_test.status, ConnectionStatus::Passed);
        assert_eq!(server.finish().len(), 2);
    }
}
