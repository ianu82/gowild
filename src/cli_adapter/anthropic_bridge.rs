use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT};
use reqwest::{StatusCode, Url};
use serde_json::{json, Map, Value};

#[cfg(test)]
use super::bridge_http::BridgeRequest;
use super::bridge_http::{
    read_request, route_token, write_response, MAX_RESPONSE_BODY_BYTES, UPSTREAM_INFERENCE_TIMEOUT,
};
use super::ResolvedGateway;
use crate::gateway::{GatewayPreset, GatewayProtocol, MINDSHUB_ANTHROPIC_BASE_URL};

const IO_TIMEOUT: Duration = Duration::from_secs(180);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Keeps a loopback-only compatibility bridge alive for managed Claude routes.
///
/// MindsHub requires an identifiable HTTP client and can occasionally return a
/// complete Anthropic message body for a streaming request. The bridge makes
/// the upstream request non-streaming, supplies GoWild's request identity, and
/// emits the equivalent Anthropic event stream expected by Claude Code.
pub(crate) struct AnthropicBridge {
    local_base_url: String,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AnthropicBridge {
    pub(crate) fn start_required(resolved: &ResolvedGateway) -> Result<Self, String> {
        debug_assert!(Self::is_required(resolved));
        let mut forwarded_headers = BTreeSet::from([
            "authorization".to_string(),
            "content-type".to_string(),
            "x-api-key".to_string(),
            "anthropic-beta".to_string(),
            "anthropic-version".to_string(),
        ]);
        if let Some(name) = &resolved.gateway.auth.header_name {
            forwarded_headers.insert(name.to_ascii_lowercase());
        }
        forwarded_headers.extend(
            resolved
                .gateway
                .custom_headers
                .keys()
                .map(|name| name.to_ascii_lowercase()),
        );
        Self::start(resolved.endpoint.clone(), forwarded_headers)
    }

    fn start(
        upstream_base_url: String,
        forwarded_headers: BTreeSet<String>,
    ) -> Result<Self, String> {
        let upstream = Url::parse(&upstream_base_url)
            .map_err(|_| "the MindsHub Anthropic endpoint is invalid".to_string())?;
        let messages_url = upstream
            .join("/v1/messages")
            .map_err(|_| "the MindsHub Anthropic endpoint cannot be adapted".to_string())?;
        let count_tokens_url = upstream
            .join("/v1/messages/count_tokens")
            .map_err(|_| "the MindsHub Anthropic endpoint cannot be adapted".to_string())?;
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|_| "GoWild could not bind the local MindsHub Anthropic bridge".to_string())?;
        listener.set_nonblocking(true).map_err(|_| {
            "GoWild could not configure the local MindsHub Anthropic bridge".to_string()
        })?;
        let address = listener.local_addr().map_err(|_| {
            "GoWild could not inspect the local MindsHub Anthropic bridge".to_string()
        })?;
        let route_token = route_token(address.port());
        let expected_messages_path = format!("/{route_token}/v1/messages");
        let expected_count_tokens_path = format!("/{route_token}/v1/messages/count_tokens");
        let local_base_url = format!("http://{address}/{route_token}");
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let client = match Client::builder()
                .timeout(UPSTREAM_INFERENCE_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("gowild/", env!("CARGO_PKG_VERSION")))
                .build()
            {
                Ok(client) => client,
                Err(_) => {
                    let _ = ready_tx.send(Err(()));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            run_bridge(
                listener,
                client,
                messages_url,
                count_tokens_url,
                expected_messages_path,
                expected_count_tokens_path,
                forwarded_headers,
                thread_shutdown,
            );
        });
        ready_rx
            .recv()
            .map_err(|_| "GoWild could not initialize the MindsHub Anthropic bridge".to_string())?
            .map_err(|()| {
                "GoWild could not initialize the MindsHub Anthropic bridge".to_string()
            })?;

        Ok(Self {
            local_base_url,
            shutdown,
            thread: Some(thread),
        })
    }

    pub(crate) fn is_required(resolved: &ResolvedGateway) -> bool {
        resolved.protocol == GatewayProtocol::AnthropicMessages
            && resolved.gateway.preset == Some(GatewayPreset::MindsHubInference)
            && resolved.endpoint.trim_end_matches('/') == MINDSHUB_ANTHROPIC_BASE_URL
    }

    pub(crate) fn local_base_url(&self) -> &str {
        &self.local_base_url
    }
}

impl Drop for AnthropicBridge {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl fmt::Debug for AnthropicBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicBridge")
            .field("local_base_url", &self.local_base_url)
            .finish_non_exhaustive()
    }
}

#[allow(clippy::too_many_arguments)]
fn run_bridge(
    listener: TcpListener,
    client: Client,
    messages_url: Url,
    count_tokens_url: Url,
    expected_messages_path: String,
    expected_count_tokens_path: String,
    forwarded_headers: BTreeSet<String>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let client = client.clone();
                let messages_url = messages_url.clone();
                let count_tokens_url = count_tokens_url.clone();
                let expected_messages_path = expected_messages_path.clone();
                let expected_count_tokens_path = expected_count_tokens_path.clone();
                let forwarded_headers = forwarded_headers.clone();
                std::thread::spawn(move || {
                    let _ = handle_connection(
                        stream,
                        &client,
                        &messages_url,
                        &count_tokens_url,
                        &expected_messages_path,
                        &expected_count_tokens_path,
                        &forwarded_headers,
                    );
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => return,
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    client: &Client,
    messages_url: &Url,
    count_tokens_url: &Url,
    expected_messages_path: &str,
    expected_count_tokens_path: &str,
    forwarded_headers: &BTreeSet<String>,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            if error.should_respond() {
                write_anthropic_error(&mut stream, StatusCode::BAD_REQUEST, error.message())?;
            }
            return Ok(());
        }
    };
    if request.method != "POST" {
        write_anthropic_error(&mut stream, StatusCode::NOT_FOUND, "route not found")?;
        return Ok(());
    }
    let (request_path, request_query) = request
        .path
        .split_once('?')
        .map_or((request.path.as_str(), None), |(path, query)| {
            (path, Some(query))
        });
    let is_messages = request_path == expected_messages_path;
    let is_count_tokens = request_path == expected_count_tokens_path;
    if !is_messages && !is_count_tokens {
        write_anthropic_error(&mut stream, StatusCode::NOT_FOUND, "route not found")?;
        return Ok(());
    }

    let mut payload: Value = match serde_json::from_slice(&request.body) {
        Ok(payload) => payload,
        Err(_) => {
            write_anthropic_error(&mut stream, StatusCode::BAD_REQUEST, "invalid JSON request")?;
            return Ok(());
        }
    };
    let client_requested_stream = is_messages
        && payload
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if is_messages {
        let Some(object) = payload.as_object_mut() else {
            write_anthropic_error(
                &mut stream,
                StatusCode::BAD_REQUEST,
                "invalid Anthropic request",
            )?;
            return Ok(());
        };
        object.insert("stream".to_string(), Value::Bool(false));
    }
    let upstream_body = match serde_json::to_vec(&payload) {
        Ok(body) => body,
        Err(_) => {
            write_anthropic_error(
                &mut stream,
                StatusCode::BAD_REQUEST,
                "invalid Anthropic request",
            )?;
            return Ok(());
        }
    };

    let mut headers = forwarded_header_map(request.headers, forwarded_headers);
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    if !headers.contains_key("anthropic-version") {
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
    }
    let mut upstream_url = if is_messages {
        messages_url.clone()
    } else {
        count_tokens_url.clone()
    };
    upstream_url.set_query(request_query);
    let response = match client
        .post(upstream_url)
        .headers(headers)
        .body(upstream_body)
        .send()
    {
        Ok(response) => response,
        Err(_) => {
            write_anthropic_error(
                &mut stream,
                StatusCode::BAD_GATEWAY,
                "MindsHub Inference did not respond",
            )?;
            return Ok(());
        }
    };
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let response_body = match response.bytes() {
        Ok(body) if body.len() <= MAX_RESPONSE_BODY_BYTES => body.to_vec(),
        Ok(_) => {
            write_anthropic_error(
                &mut stream,
                StatusCode::BAD_GATEWAY,
                "MindsHub Inference returned an oversized response",
            )?;
            return Ok(());
        }
        Err(_) => {
            write_anthropic_error(
                &mut stream,
                StatusCode::BAD_GATEWAY,
                "MindsHub Inference returned an incomplete response",
            )?;
            return Ok(());
        }
    };

    if status.is_success() && client_requested_stream {
        if content_type.starts_with("text/event-stream") {
            write_response(&mut stream, status, "text/event-stream", &response_body)
        } else {
            let sse = match message_to_sse(&response_body) {
                Ok(sse) => sse,
                Err(()) => {
                    write_anthropic_error(
                        &mut stream,
                        StatusCode::BAD_GATEWAY,
                        "MindsHub Inference returned an invalid Anthropic payload",
                    )?;
                    return Ok(());
                }
            };
            write_response(&mut stream, status, "text/event-stream", &sse)
        }
    } else {
        write_response(&mut stream, status, &content_type, &response_body)
    }
}

fn forwarded_header_map(
    request_headers: Vec<(String, String)>,
    forwarded_headers: &BTreeSet<String>,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in request_headers {
        if !forwarded_headers.contains(&name) {
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_str(&value) else {
            continue;
        };
        headers.append(name, value);
    }
    headers
}

fn message_to_sse(body: &[u8]) -> Result<Vec<u8>, ()> {
    let message: Value = serde_json::from_slice(body).map_err(|_| ())?;
    let object = message.as_object().ok_or(())?;
    if object.get("type").and_then(Value::as_str) != Some("message") {
        return Err(());
    }
    let content = object.get("content").and_then(Value::as_array).ok_or(())?;

    let mut start_message = message.clone();
    let start_object = start_message.as_object_mut().ok_or(())?;
    start_object.insert("content".into(), Value::Array(Vec::new()));
    start_object.insert("stop_reason".into(), Value::Null);
    start_object.insert("stop_sequence".into(), Value::Null);
    let input_tokens = object
        .get("usage")
        .and_then(Value::as_object)
        .and_then(|usage| usage.get("input_tokens"))
        .cloned()
        .unwrap_or_else(|| Value::from(0));
    start_object.insert(
        "usage".into(),
        json!({"input_tokens": input_tokens, "output_tokens": 0}),
    );

    let mut events = vec![
        json!({"type": "message_start", "message": start_message}),
        json!({"type": "ping"}),
    ];
    for (index, block) in content.iter().enumerate() {
        let block_type = block.get("type").and_then(Value::as_str).ok_or(())?;
        let start_block = match block_type {
            "text" => json!({"type": "text", "text": ""}),
            "tool_use" => json!({
                "type": "tool_use",
                "id": block.get("id").cloned().unwrap_or(Value::Null),
                "name": block.get("name").cloned().unwrap_or(Value::Null),
                "input": {},
            }),
            "thinking" => json!({"type": "thinking", "thinking": "", "signature": ""}),
            _ => block.clone(),
        };
        events.push(json!({
            "type": "content_block_start",
            "index": index,
            "content_block": start_block,
        }));
        match block_type {
            "text" => events.push(json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "text_delta",
                    "text": block.get("text").and_then(Value::as_str).unwrap_or(""),
                },
            })),
            "tool_use" => events.push(json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": serde_json::to_string(
                        block.get("input").unwrap_or(&Value::Object(Map::new()))
                    ).map_err(|_| ())?,
                },
            })),
            "thinking" => {
                if let Some(thinking) = block.get("thinking").and_then(Value::as_str) {
                    events.push(json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "thinking_delta", "thinking": thinking},
                    }));
                }
                if let Some(signature) = block.get("signature").and_then(Value::as_str) {
                    events.push(json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "signature_delta", "signature": signature},
                    }));
                }
            }
            _ => {}
        }
        events.push(json!({"type": "content_block_stop", "index": index}));
    }

    let stop_reason = object.get("stop_reason").cloned().unwrap_or(Value::Null);
    let stop_sequence = object.get("stop_sequence").cloned().unwrap_or(Value::Null);
    let output_tokens = object
        .get("usage")
        .and_then(Value::as_object)
        .and_then(|usage| usage.get("output_tokens"))
        .cloned()
        .unwrap_or_else(|| Value::from(0));
    events.push(json!({
        "type": "message_delta",
        "delta": {"stop_reason": stop_reason, "stop_sequence": stop_sequence},
        "usage": {"output_tokens": output_tokens},
    }));
    events.push(json!({"type": "message_stop"}));

    let mut sse = Vec::new();
    for event in events {
        let event_type = event.get("type").and_then(Value::as_str).ok_or(())?;
        write!(&mut sse, "event: {event_type}\ndata: ").map_err(|_| ())?;
        serde_json::to_writer(&mut sse, &event).map_err(|_| ())?;
        sse.extend_from_slice(b"\n\n");
    }
    Ok(sse)
}

fn write_anthropic_error(
    stream: &mut TcpStream,
    status: StatusCode,
    message: &str,
) -> io::Result<()> {
    let body = serde_json::to_vec(&json!({
        "type": "error",
        "error": {"type": "api_error", "message": message},
    }))
    .unwrap_or_else(|_| {
        b"{\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"bridge error\"}}"
            .to_vec()
    });
    write_response(stream, status, "application/json", &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::Gateway;

    fn mock_upstream(response_body: Value) -> (String, std::sync::mpsc::Receiver<BridgeRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream).unwrap();
            sender.send(request).unwrap();
            let body = serde_json::to_vec(&response_body).unwrap();
            write_response(&mut stream, StatusCode::OK, "application/json", &body).unwrap();
        });
        (format!("http://{address}"), receiver)
    }

    #[test]
    fn official_mindshub_anthropic_route_requires_bridge() {
        let resolved = ResolvedGateway {
            gateway: Gateway::mindshub(),
            protocol: GatewayProtocol::AnthropicMessages,
            endpoint: MINDSHUB_ANTHROPIC_BASE_URL.into(),
            credential: None,
            model: Some("gpt-codex".into()),
        };
        assert!(AnthropicBridge::is_required(&resolved));

        let mut custom = resolved;
        custom.endpoint = "https://gateway.example".into();
        assert!(!AnthropicBridge::is_required(&custom));
    }

    #[test]
    fn nonstreaming_message_is_returned_as_anthropic_sse() {
        let upstream_response = json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "model": "gpt-codex",
            "content": [
                {"type": "text", "text": "READY"},
                {"type": "tool_use", "id": "tool_test", "name": "Read", "input": {"path": "TASK.md"}},
            ],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": {"input_tokens": 3, "output_tokens": 4},
        });
        let (upstream, received) = mock_upstream(upstream_response);
        let bridge = AnthropicBridge::start(
            upstream,
            BTreeSet::from(["authorization".to_string(), "content-type".to_string()]),
        )
        .unwrap();
        let response = Client::new()
            .post(format!("{}/v1/messages?beta=true", bridge.local_base_url()))
            .bearer_auth("test-secret")
            .json(&json!({
                "model": "gpt-codex",
                "messages": [{"role": "user", "content": "read a file"}],
                "max_tokens": 100,
                "stream": true,
            }))
            .send()
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .unwrap(),
            "text/event-stream"
        );
        let body = response.text().unwrap();
        assert!(body.contains("event: message_start"));
        assert!(body.contains("event: content_block_delta"));
        assert!(body.contains("\"text\":\"READY\""));
        assert!(body.contains("\"partial_json\":\"{\\\"path\\\":\\\"TASK.md\\\"}\""));
        assert!(body.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));

        let forwarded = received.recv_timeout(Duration::from_secs(1)).unwrap();
        let forwarded_payload: Value = serde_json::from_slice(&forwarded.body).unwrap();
        assert_eq!(forwarded_payload["stream"], false);
        assert_eq!(forwarded.path, "/v1/messages?beta=true");
        assert_eq!(
            forwarded
                .headers
                .iter()
                .find(|(name, _)| name == "authorization")
                .map(|(_, value)| value.as_str()),
            Some("Bearer test-secret")
        );
        assert_eq!(
            forwarded
                .headers
                .iter()
                .find(|(name, _)| name == "user-agent")
                .map(|(_, value)| value.as_str()),
            Some(concat!("gowild/", env!("CARGO_PKG_VERSION")))
        );
    }
}
