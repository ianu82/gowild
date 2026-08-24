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
use serde_json::{json, Value};

#[cfg(test)]
use super::bridge_http::BridgeRequest;
use super::bridge_http::{
    read_request, route_token, write_error, write_response, MAX_RESPONSE_BODY_BYTES,
    UPSTREAM_INFERENCE_TIMEOUT,
};
use super::ResolvedGateway;
use crate::gateway::{GatewayPreset, GatewayProtocol, MINDSHUB_RESPONSES_BASE_URL};

const IO_TIMEOUT: Duration = Duration::from_secs(180);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Keeps a loopback-only compatibility bridge alive for one managed Codex route.
///
/// MindsHub's Responses endpoint currently accepts non-streaming requests but
/// returns an upstream error for the streaming requests Codex emits. The bridge
/// changes only that transport detail: it requests a complete response from
/// MindsHub, then emits the equivalent Responses SSE events to Codex. Prompts,
/// tools, model selection, and credentials otherwise pass through unchanged.
pub(crate) struct ResponsesBridge {
    local_base_url: String,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ResponsesBridge {
    pub(crate) fn start_required(resolved: &ResolvedGateway) -> Result<Self, String> {
        debug_assert!(Self::is_required(resolved));
        let mut forwarded_headers = BTreeSet::from([
            "authorization".to_string(),
            "content-type".to_string(),
            "x-api-key".to_string(),
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
            .map_err(|_| "the MindsHub Responses endpoint is invalid".to_string())?;
        let upstream_url = upstream
            .join(&format!(
                "{}/responses",
                upstream.path().trim_end_matches('/')
            ))
            .map_err(|_| "the MindsHub Responses endpoint cannot be adapted".to_string())?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|_| {
            "GoWild could not bind the local MindsHub compatibility bridge".to_string()
        })?;
        listener.set_nonblocking(true).map_err(|_| {
            "GoWild could not configure the local MindsHub compatibility bridge".to_string()
        })?;
        let address = listener.local_addr().map_err(|_| {
            "GoWild could not inspect the local MindsHub compatibility bridge".to_string()
        })?;
        let route_token = route_token(address.port());
        let expected_path = format!("/{route_token}/v1/responses");
        let local_base_url = format!("http://{address}/{route_token}/v1");
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
                upstream_url,
                expected_path,
                forwarded_headers,
                thread_shutdown,
            )
        });
        ready_rx
            .recv()
            .map_err(|_| {
                "GoWild could not initialize the MindsHub compatibility bridge".to_string()
            })?
            .map_err(|()| {
                "GoWild could not initialize the MindsHub compatibility bridge".to_string()
            })?;

        Ok(Self {
            local_base_url,
            shutdown,
            thread: Some(thread),
        })
    }

    pub(crate) fn is_required(resolved: &ResolvedGateway) -> bool {
        requires_bridge(resolved)
    }

    pub(crate) fn local_base_url(&self) -> &str {
        &self.local_base_url
    }
}

impl Drop for ResponsesBridge {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl fmt::Debug for ResponsesBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponsesBridge")
            .field("local_base_url", &self.local_base_url)
            .finish_non_exhaustive()
    }
}

fn requires_bridge(resolved: &ResolvedGateway) -> bool {
    resolved.protocol == GatewayProtocol::OpenAiResponses
        && resolved.gateway.preset == Some(GatewayPreset::MindsHubInference)
        && resolved.endpoint.trim_end_matches('/') == MINDSHUB_RESPONSES_BASE_URL
}

fn run_bridge(
    listener: TcpListener,
    client: Client,
    upstream_url: Url,
    expected_path: String,
    forwarded_headers: BTreeSet<String>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let client = client.clone();
                let upstream_url = upstream_url.clone();
                let expected_path = expected_path.clone();
                let forwarded_headers = forwarded_headers.clone();
                std::thread::spawn(move || {
                    let _ = handle_connection(
                        stream,
                        &client,
                        &upstream_url,
                        &expected_path,
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
    upstream_url: &Url,
    expected_path: &str,
    forwarded_headers: &BTreeSet<String>,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            if error.should_respond() {
                write_error(&mut stream, StatusCode::BAD_REQUEST, error.message())?;
            }
            return Ok(());
        }
    };
    if request.method != "POST" || request.path != expected_path {
        write_error(&mut stream, StatusCode::NOT_FOUND, "route not found")?;
        return Ok(());
    }

    let mut payload: Value = match serde_json::from_slice(&request.body) {
        Ok(payload) => payload,
        Err(_) => {
            write_error(&mut stream, StatusCode::BAD_REQUEST, "invalid JSON request")?;
            return Ok(());
        }
    };
    let client_requested_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(object) = payload.as_object_mut() else {
        write_error(
            &mut stream,
            StatusCode::BAD_REQUEST,
            "invalid Responses request",
        )?;
        return Ok(());
    };
    object.insert("stream".to_string(), Value::Bool(false));
    let upstream_body = match serde_json::to_vec(&payload) {
        Ok(body) => body,
        Err(_) => {
            write_error(
                &mut stream,
                StatusCode::BAD_REQUEST,
                "invalid Responses request",
            )?;
            return Ok(());
        }
    };

    let mut headers = HeaderMap::new();
    for (name, value) in request.headers {
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
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

    let response = match client
        .post(upstream_url.clone())
        .headers(headers)
        .body(upstream_body)
        .send()
    {
        Ok(response) => response,
        Err(_) => {
            write_error(
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
            write_error(
                &mut stream,
                StatusCode::BAD_GATEWAY,
                "MindsHub Inference returned an oversized response",
            )?;
            return Ok(());
        }
        Err(_) => {
            write_error(
                &mut stream,
                StatusCode::BAD_GATEWAY,
                "MindsHub Inference returned an incomplete response",
            )?;
            return Ok(());
        }
    };

    if status.is_success() && client_requested_stream {
        let sse = match response_to_sse(&response_body) {
            Ok(sse) => sse,
            Err(()) => {
                write_error(
                    &mut stream,
                    StatusCode::BAD_GATEWAY,
                    "MindsHub Inference returned an invalid Responses payload",
                )?;
                return Ok(());
            }
        };
        write_response(&mut stream, status, "text/event-stream", &sse)
    } else {
        write_response(&mut stream, status, &content_type, &response_body)
    }
}

fn response_to_sse(body: &[u8]) -> Result<Vec<u8>, ()> {
    let response: Value = serde_json::from_slice(body).map_err(|_| ())?;
    let response_object = response.as_object().ok_or(())?;
    let output = response_object
        .get("output")
        .and_then(Value::as_array)
        .ok_or(())?;
    // Some compatible gateways echo the complete request (including every
    // instruction and tool schema) in the response object. Repeating that
    // object in lifecycle events creates enormous single-line SSE frames that
    // Codex's HTTP decoder rejects. Codex only consumes the lifecycle fields
    // below; output items are delivered by their dedicated events.
    let lifecycle_response = |status: Option<&str>| {
        let mut compact = serde_json::Map::new();
        for key in [
            "id",
            "object",
            "created_at",
            "model",
            "status",
            "usage",
            "error",
            "incomplete_details",
            "end_turn",
        ] {
            if let Some(value) = response_object.get(key) {
                compact.insert(key.to_string(), value.clone());
            }
        }
        if let Some(status) = status {
            compact.insert("status".to_string(), Value::String(status.into()));
        }
        compact.insert("output".to_string(), Value::Array(Vec::new()));
        Value::Object(compact)
    };
    let initial_response = lifecycle_response(Some("in_progress"));

    let mut events = vec![
        json!({"type": "response.created", "response": initial_response}),
        json!({"type": "response.in_progress", "response": initial_response}),
    ];
    for (output_index, item) in output.iter().enumerate() {
        let item_id = item.get("id").cloned().unwrap_or(Value::Null);
        events.push(json!({
            "type": "response.output_item.added",
            "output_index": output_index,
            "item": item,
        }));
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("");
                events.push(json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": item_id,
                    "output_index": output_index,
                    "delta": arguments,
                }));
                events.push(json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": item_id,
                    "output_index": output_index,
                    "arguments": arguments,
                }));
            }
            Some("message") => {
                if let Some(content) = item.get("content").and_then(Value::as_array) {
                    for (content_index, part) in content.iter().enumerate() {
                        events.push(json!({
                            "type": "response.content_part.added",
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": content_index,
                            "part": part,
                        }));
                        if part.get("type").and_then(Value::as_str) == Some("output_text") {
                            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                            events.push(json!({
                                "type": "response.output_text.delta",
                                "item_id": item_id,
                                "output_index": output_index,
                                "content_index": content_index,
                                "delta": text,
                            }));
                            events.push(json!({
                                "type": "response.output_text.done",
                                "item_id": item_id,
                                "output_index": output_index,
                                "content_index": content_index,
                                "text": text,
                            }));
                        }
                        events.push(json!({
                            "type": "response.content_part.done",
                            "item_id": item_id,
                            "output_index": output_index,
                            "content_index": content_index,
                            "part": part,
                        }));
                    }
                }
            }
            _ => {}
        }
        events.push(json!({
            "type": "response.output_item.done",
            "output_index": output_index,
            "item": item,
        }));
    }
    let final_event = match response_object.get("status").and_then(Value::as_str) {
        Some("incomplete") => "response.incomplete",
        Some("failed") => "response.failed",
        _ => "response.completed",
    };
    events.push(json!({"type": final_event, "response": lifecycle_response(None)}));

    let mut sse = Vec::new();
    for event in events {
        let event_type = event.get("type").and_then(Value::as_str).ok_or(())?;
        write!(&mut sse, "event: {event_type}\ndata: ").map_err(|_| ())?;
        serde_json::to_writer(&mut sse, &event).map_err(|_| ())?;
        sse.extend_from_slice(b"\n\n");
    }
    Ok(sse)
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
        (format!("http://{address}/v1"), receiver)
    }

    #[test]
    fn official_mindshub_responses_route_requires_bridge() {
        let resolved = ResolvedGateway {
            gateway: Gateway::mindshub(),
            protocol: GatewayProtocol::OpenAiResponses,
            endpoint: MINDSHUB_RESPONSES_BASE_URL.into(),
            credential: None,
            model: Some("deepseek".into()),
        };
        assert!(requires_bridge(&resolved));

        let mut custom = resolved;
        custom.endpoint = "https://gateway.example/v1".into();
        assert!(!requires_bridge(&custom));
    }

    #[test]
    fn streaming_request_is_forwarded_nonstreaming_and_returned_as_sse() {
        let upstream_response = json!({
            "id": "resp_test",
            "object": "response",
            "status": "completed",
            "output": [{
                "id": "msg_test",
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "READY", "annotations": []}],
            }],
            "usage": {"input_tokens": 3, "output_tokens": 1, "total_tokens": 4},
        });
        let (upstream, received) = mock_upstream(upstream_response);
        let bridge = ResponsesBridge::start(
            upstream,
            BTreeSet::from(["authorization".to_string(), "content-type".to_string()]),
        )
        .unwrap();
        let response = Client::new()
            .post(format!("{}/responses", bridge.local_base_url()))
            .bearer_auth("test-secret")
            .json(&json!({
                "model": "deepseek",
                "input": "read a file",
                "tools": [{
                    "type": "function",
                    "name": "exec_command",
                    "description": "run a command",
                    "parameters": {"type": "object"},
                }],
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
        assert!(body.contains("event: response.output_text.delta"));
        assert!(body.contains("\"delta\":\"READY\""));
        assert!(body.contains("event: response.completed"));
        assert!(body.ends_with("\n\n"));
        assert!(!body.contains("[DONE]"));

        let forwarded = received.recv_timeout(Duration::from_secs(1)).unwrap();
        let forwarded_payload: Value = serde_json::from_slice(&forwarded.body).unwrap();
        assert_eq!(forwarded_payload["stream"], false);
        assert_eq!(forwarded_payload["input"], "read a file");
        assert_eq!(forwarded_payload["tools"][0]["name"], "exec_command");
        assert_eq!(forwarded.path, "/v1/responses");
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

    #[test]
    fn function_call_arguments_are_emitted_for_codex_tool_loops() {
        let sse = response_to_sse(
            &serde_json::to_vec(&json!({
                "id": "resp_tool",
                "object": "response",
                "status": "completed",
                "output": [{
                    "id": "fc_test",
                    "type": "function_call",
                    "call_id": "call_test",
                    "name": "shell_command",
                    "arguments": "{\"command\":\"cat route-proof.txt\"}",
                    "status": "completed",
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        let sse = String::from_utf8(sse).unwrap();
        assert!(sse.contains("event: response.function_call_arguments.delta"));
        assert!(sse.contains("event: response.function_call_arguments.done"));
        assert!(sse.contains("cat route-proof.txt"));
    }

    #[test]
    fn lifecycle_events_do_not_repeat_large_gateway_echoes() {
        let echoed_instructions = "x".repeat(256 * 1024);
        let sse = response_to_sse(
            &serde_json::to_vec(&json!({
                "id": "resp_large",
                "object": "response",
                "status": "completed",
                "model": "gpt-codex",
                "instructions": echoed_instructions,
                "tools": [{"description": "y".repeat(256 * 1024)}],
                "output": [{
                    "id": "msg_large",
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": "READY"}],
                }],
                "usage": {"input_tokens": 3, "output_tokens": 1, "total_tokens": 4},
            }))
            .unwrap(),
        )
        .unwrap();
        let sse = String::from_utf8(sse).unwrap();

        assert!(sse.len() < 16 * 1024);
        assert!(!sse.contains(&"x".repeat(1024)));
        assert!(!sse.contains(&"y".repeat(1024)));
        assert!(sse.contains("event: response.completed"));
        assert!(sse.contains("\"id\":\"resp_large\""));
    }
}
