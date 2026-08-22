#!/usr/bin/env python3
"""Exercise installed Codex and Claude CLIs against a loopback gateway."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import threading
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


FAKE_MODEL = "gowild-routing-check-model"
FAKE_SECRET = "gowild-routing-check-secret"
LOGIN_MARKERS = (
    "authentication required",
    "log in to claude",
    "login required",
    "not logged in",
    "please log in",
    "run /login",
    "sign in to claude",
)


@dataclass(frozen=True)
class CapturedRequest:
    path: str
    model: str | None
    stream: bool | None
    auth_headers: tuple[str, ...]
    credential_match: bool
    user_agent: str


class RecordingServer(ThreadingHTTPServer):
    def __init__(self) -> None:
        super().__init__(("127.0.0.1", 0), RecordingHandler)
        self.requests: list[CapturedRequest] = []
        self.requests_lock = threading.Lock()
        self.codex_tool_output_seen = False
        self.codex_tool_secret_seen = False
        self.codex_tool_environment_filtered = False
        self.codex_tool_environment_leaked = False


class RecordingHandler(BaseHTTPRequestHandler):
    server_version = "GoWildRoutingCheck/1.0"

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        payload = b'{"models":[]}'
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        length = int(self.headers.get("content-length", "0"))
        body = parse_json(self.rfile.read(length))
        auth_headers = tuple(
            name
            for name in ("authorization", "x-api-key", "api-key")
            if self.headers.get(name)
        )
        credential_match = any(
            FAKE_SECRET in self.headers.get(name, "") for name in auth_headers
        )
        request = CapturedRequest(
            path=self.path,
            model=body.get("model"),
            stream=body.get("stream"),
            auth_headers=auth_headers,
            credential_match=credential_match,
            user_agent=self.headers.get("user-agent", ""),
        )
        server: RecordingServer = self.server  # type: ignore[assignment]
        with server.requests_lock:
            server.requests.append(request)

        if "codex" in request.user_agent.lower():
            tool_outputs = [
                item.get("output")
                for item in body.get("input", [])
                if isinstance(item, dict) and item.get("type") == "function_call_output"
            ]
            serialized_tool_outputs = json.dumps(tool_outputs)
            tool_output_seen = bool(tool_outputs)
            with server.requests_lock:
                server.codex_tool_output_seen |= tool_output_seen
                server.codex_tool_secret_seen |= (
                    tool_output_seen and FAKE_SECRET in serialized_tool_outputs
                )
                server.codex_tool_environment_filtered |= (
                    tool_output_seen and "GOWILD_ENV_FILTERED" in serialized_tool_outputs
                )
                server.codex_tool_environment_leaked |= (
                    tool_output_seen and "GOWILD_ENV_LEAKED" in serialized_tool_outputs
                )
            self.send_codex_response(tool_output_seen)
            return

        payload = json.dumps(
            {
                "type": "error",
                "error": {
                    "type": "gateway_test_complete",
                    "message": "GoWild loopback routing capture complete",
                },
            }
        ).encode()
        self.send_response(418)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def send_codex_response(self, tool_output_seen: bool) -> None:
        if tool_output_seen:
            item = {
                "id": "msg_gowild_routing_check",
                "type": "message",
                "role": "assistant",
                "status": "completed",
                "content": [
                    {"type": "output_text", "text": "ROUTED", "annotations": []}
                ],
            }
        else:
            command = (
                "printf 'ROUTED' > route-proof.txt && cat route-proof.txt && "
                "if [ -z \"${GOWILD_CODEX_API_KEY+x}\" ] && "
                "[ -z \"${GOWILD_API_KEY+x}\" ]; then "
                "printf '\\nGOWILD_ENV_FILTERED\\n'; else "
                "printf '\\nGOWILD_ENV_LEAKED\\n'; fi"
            )
            item = {
                "id": "fc_gowild_routing_check",
                "type": "function_call",
                "call_id": "call_gowild_routing_check",
                "name": "shell_command",
                "arguments": json.dumps({"command": command}),
                "status": "completed",
            }

        response = {
            "id": "resp_gowild_routing_check",
            "object": "response",
            "status": "completed",
            "model": FAKE_MODEL,
            "output": [item],
            "parallel_tool_calls": True,
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
        }
        initial = dict(response)
        initial["status"] = "in_progress"
        initial["output"] = []
        events: list[dict[str, Any]] = [
            {"type": "response.created", "response": initial},
            {"type": "response.in_progress", "response": initial},
            {"type": "response.output_item.added", "output_index": 0, "item": item},
        ]
        if item["type"] == "function_call":
            events.extend(
                [
                    {
                        "type": "response.function_call_arguments.delta",
                        "item_id": item["id"],
                        "output_index": 0,
                        "delta": item["arguments"],
                    },
                    {
                        "type": "response.function_call_arguments.done",
                        "item_id": item["id"],
                        "output_index": 0,
                        "arguments": item["arguments"],
                    },
                ]
            )
        else:
            part = item["content"][0]
            events.extend(
                [
                    {
                        "type": "response.content_part.added",
                        "item_id": item["id"],
                        "output_index": 0,
                        "content_index": 0,
                        "part": part,
                    },
                    {
                        "type": "response.output_text.delta",
                        "item_id": item["id"],
                        "output_index": 0,
                        "content_index": 0,
                        "delta": "ROUTED",
                    },
                    {
                        "type": "response.output_text.done",
                        "item_id": item["id"],
                        "output_index": 0,
                        "content_index": 0,
                        "text": "ROUTED",
                    },
                    {
                        "type": "response.content_part.done",
                        "item_id": item["id"],
                        "output_index": 0,
                        "content_index": 0,
                        "part": part,
                    },
                ]
            )
        events.extend(
            [
                {"type": "response.output_item.done", "output_index": 0, "item": item},
                {"type": "response.completed", "response": response},
            ]
        )
        payload = "".join(
            f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events
        ) + "data: [DONE]\n\n"
        encoded = payload.encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, _format: str, *_args: object) -> None:
        return


def parse_json(raw: bytes) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def command_environment(
    *,
    remove: tuple[str, ...],
    set_values: dict[str, str],
) -> dict[str, str]:
    environment = os.environ.copy()
    for key in remove:
        environment.pop(key, None)
    environment.update(set_values)
    return environment


def run_command(
    command: list[str], environment: dict[str, str], *, cwd: str | None = None
) -> str:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            env=environment,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(f"{command[0]} did not reach the loopback gateway in time") from error
    output = result.stdout + result.stderr
    if FAKE_SECRET in output:
        raise RuntimeError(f"{command[0]} exposed the gateway credential in output")
    if any(marker in output.lower() for marker in LOGIN_MARKERS):
        raise RuntimeError(f"{command[0]} requested proprietary login:\n{output}")
    return output


def codex_command(executable: str, base_url: str) -> list[str]:
    return [
        executable,
        "-c",
        "model_providers.gowild={}",
        "-c",
        'model_providers.gowild.name="GoWild Loopback"',
        "-c",
        f'model_providers.gowild.base_url="{base_url}/v1"',
        "-c",
        'model_providers.gowild.wire_api="responses"',
        "-c",
        "model_providers.gowild.requires_openai_auth=false",
        "-c",
        'model_providers.gowild.env_key="GOWILD_CODEX_API_KEY"',
        "-c",
        "features.shell_snapshot=false",
        "-c",
        "shell_environment_policy.ignore_default_excludes=false",
        "-c",
        'model_provider="gowild"',
        "-c",
        f'model="{FAKE_MODEL}"',
        "exec",
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
        "--sandbox",
        "workspace-write",
        "--color",
        "never",
        "Reply with only ROUTED.",
    ]


def claude_command(executable: str) -> list[str]:
    return [
        executable,
        "--print",
        "--bare",
        "--output-format",
        "text",
        "--no-session-persistence",
        "--tools",
        "",
        "--model",
        FAKE_MODEL,
        "Reply with only ROUTED.",
    ]


def require_request(
    requests: list[CapturedRequest],
    *,
    path: str,
    user_agent_marker: str,
) -> None:
    matching = [
        request
        for request in requests
        if request.path.split("?", maxsplit=1)[0] == path
        and user_agent_marker.lower() in request.user_agent.lower()
    ]
    if not matching:
        raise RuntimeError(f"no {user_agent_marker} request reached {path}")
    request = matching[0]
    if request.model != FAKE_MODEL:
        raise RuntimeError(
            f"{user_agent_marker} sent model {request.model!r}, expected {FAKE_MODEL!r}"
        )
    if request.stream is not True:
        raise RuntimeError(f"{user_agent_marker} did not request a streamed response")
    if not request.auth_headers:
        raise RuntimeError(f"{user_agent_marker} omitted gateway authentication")
    if not request.credential_match:
        raise RuntimeError(f"{user_agent_marker} used a credential other than the gateway key")


def require_secret_absent(root: Path) -> None:
    encoded = FAKE_SECRET.encode()
    for path in root.rglob("*"):
        if path.is_file() and encoded in path.read_bytes():
            raise RuntimeError(f"Codex persisted the gateway credential in {path.relative_to(root)}")


def main() -> None:
    codex = shutil.which("codex")
    claude = shutil.which("claude")
    if not codex or not claude:
        missing = ", ".join(
            name for name, executable in (("codex", codex), ("claude", claude)) if not executable
        )
        raise SystemExit(f"missing installed coding CLI: {missing}")

    server = RecordingServer()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base_url = f"http://127.0.0.1:{server.server_port}"
    try:
        with (
            tempfile.TemporaryDirectory(prefix="gowild-codex-routing-") as codex_home,
            tempfile.TemporaryDirectory(prefix="gowild-codex-worktree-") as codex_worktree,
        ):
            codex_output = run_command(
                codex_command(codex, base_url),
                command_environment(
                    remove=(
                        "OPENAI_API_KEY",
                        "CODEX_API_KEY",
                        "CODEX_ACCESS_TOKEN",
                        "GOWILD_API_KEY",
                    ),
                    set_values={
                        "CODEX_HOME": codex_home,
                        "GOWILD_CODEX_API_KEY": FAKE_SECRET,
                    },
                ),
                cwd=codex_worktree,
            )
            require_secret_absent(Path(codex_home))
            proof = Path(codex_worktree, "route-proof.txt")
            if proof.read_text() != "ROUTED":
                raise RuntimeError("Codex did not complete the loopback file-edit tool call")
        if "provider: gowild" not in codex_output:
            raise RuntimeError("Codex did not report the reserved GoWild provider")

        claude_output = run_command(
            claude_command(claude),
            command_environment(
                remove=(
                    "ANTHROPIC_API_KEY",
                    "ANTHROPIC_MODEL",
                    "CLAUDE_CODE_USE_BEDROCK",
                    "CLAUDE_CODE_USE_VERTEX",
                    "CLAUDE_CODE_USE_FOUNDRY",
                ),
                set_values={
                    "ANTHROPIC_BASE_URL": base_url,
                    "ANTHROPIC_AUTH_TOKEN": FAKE_SECRET,
                    "ANTHROPIC_CUSTOM_MODEL_OPTION": FAKE_MODEL,
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": FAKE_MODEL,
                    "CLAUDE_CODE_SUBAGENT_MODEL": FAKE_MODEL,
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
                },
            ),
        )
        if "GoWild loopback routing capture complete" not in claude_output:
            raise RuntimeError("Claude Code did not surface the loopback gateway response")

        with server.requests_lock:
            requests = list(server.requests)
            codex_tool_output_seen = server.codex_tool_output_seen
            codex_tool_secret_seen = server.codex_tool_secret_seen
            codex_tool_environment_filtered = server.codex_tool_environment_filtered
            codex_tool_environment_leaked = server.codex_tool_environment_leaked
        require_request(requests, path="/v1/responses", user_agent_marker="codex")
        require_request(requests, path="/v1/messages", user_agent_marker="claude")
        if not codex_tool_output_seen:
            raise RuntimeError("Codex did not return the loopback shell tool result")
        if codex_tool_secret_seen:
            raise RuntimeError("Codex exposed the gateway credential to its shell tool")
        if not codex_tool_environment_filtered or codex_tool_environment_leaked:
            raise RuntimeError(
                "Codex did not exclude GoWild keys from its shell environment "
                f"(filtered_marker={codex_tool_environment_filtered}, "
                f"leaked_marker={codex_tool_environment_leaked})"
            )
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)

    print("real Codex and Claude CLIs routed inference through the loopback gateway")
    print("proprietary login was not requested; gateway credentials remained redacted")


if __name__ == "__main__":
    main()
