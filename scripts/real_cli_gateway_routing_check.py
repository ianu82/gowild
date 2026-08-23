#!/usr/bin/env python3
"""Exercise installed Codex and Claude CLIs against a loopback gateway."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import threading
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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


def run_command(command: list[str], environment: dict[str, str]) -> str:
    try:
        result = subprocess.run(
            command,
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
        'model_provider="gowild"',
        "-c",
        f'model="{FAKE_MODEL}"',
        "exec",
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
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
        codex_output = run_command(
            codex_command(codex, base_url),
            command_environment(
                remove=("OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"),
                set_values={"GOWILD_CODEX_API_KEY": FAKE_SECRET},
            ),
        )
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
        require_request(requests, path="/v1/responses", user_agent_marker="codex")
        require_request(requests, path="/v1/messages", user_agent_marker="claude")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)

    print("real Codex and Claude CLIs routed inference through the loopback gateway")
    print("proprietary login was not requested; gateway credentials remained redacted")


if __name__ == "__main__":
    main()
