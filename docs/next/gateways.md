# Gateway configuration architecture

GoWild treats the coding CLI and the inference gateway as independent choices.
Gateway metadata lives in `gateways.json` under GoWild's config directory; raw
credentials never do.

The built-in MindsHub Inference preset follows the current service contracts:

- OpenAI Responses base URL: `https://api.mindshub.ai/v1`
- Anthropic Messages base URL: `https://api.mindshub.ai`
- Authenticated model discovery: `https://api.mindshub.ai/v1/models`
- Authentication: bearer token

Custom gateways may advertise OpenAI Responses, Anthropic Messages, or both.
GoWild validates each advertised protocol against a corresponding endpoint and
rejects URLs containing embedded credentials. Authentication is represented as
bearer token, `x-api-key`, a configurable secret-bearing header, or none.
Additional headers are non-secret; credential-bearing header names are rejected
there so keys cannot be written into normal configuration by mistake.

Credentials are addressed by an opaque reference such as `gateway:mindshub`.
GoWild first uses the operating system credential store (Keychain Services,
Windows Credential Manager, or Secret Service). On Unix, if that store is not
available, it can use a separate `credentials.json` fallback with a `0700`
directory and `0600` file. Windows fails closed because ordinary Unix mode bits
cannot establish an owner-only ACL there.

The credential type cannot be serialized, displays only `[REDACTED]` in debug
output, and zeroes its buffer when dropped. Connection diagnostics redact known
credentials and common key formats before persistence.

## Launch resolution

Every coding CLI declares the protocol it needs. GoWild resolves the selected
gateway before invoking the adapter and refuses to launch when that gateway
does not advertise a matching endpoint. Fresh and resumed sessions use the same
resolution path, so resume cannot silently fall back to a proprietary service.

Selection uses the following precedence:

1. An explicit per-launch gateway or model.
2. A GoWild process environment override.
3. The saved gateway and per-CLI model defaults.

The supported process overrides are:

- `GOWILD_GATEWAY`
- `GOWILD_MODEL`
- `GOWILD_API_KEY`
- `GOWILD_RESPONSES_BASE_URL`
- `GOWILD_MESSAGES_BASE_URL`

`GOWILD_API_KEY` is an ephemeral override and is never persisted. The resolved
credential is passed to an adapter as a secret value, rejected if the adapter
places it in argv or an ordinary environment value, and exposed only to the
spawned child process. Launch specifications and pane environment diagnostics
redact all environment values.

## Claude Code adapter

The Claude Code adapter requires an Anthropic Messages-compatible endpoint. It
sets `ANTHROPIC_BASE_URL`, uses `ANTHROPIC_AUTH_TOKEN` for bearer authentication
or `ANTHROPIC_API_KEY` for `x-api-key` authentication, and explicitly removes
the other credential variable. It also removes inherited Bedrock, Vertex, and
Foundry mode selectors so the chosen gateway cannot be bypassed by the parent
shell. MindsHub uses the bearer path and the host-only
`https://api.mindshub.ai` base URL.

When a model is selected, GoWild passes it through Claude's `--model` option
and exposes it as the custom model-picker entry. Background and subagent work
is pinned to the same gateway model so a custom catalog cannot fall through to
an unavailable built-in Anthropic model ID. Gateway model discovery is enabled
only when the configured catalog is available from the `/v1/models` path
Claude Code derives from the selected base URL, and resumed sessions add
`--resume <session-id>` without changing the resolved environment.

Claude Code does not provide a safe way to disable saved Anthropic credentials
while using either an unauthenticated endpoint or custom-header-only
authentication in its normal interactive mode. GoWild fails closed for those
two authentication modes instead of risking a silent provider fallback or
credential disclosure.

The adapter contract follows the current [Claude Code gateway
documentation](https://code.claude.com/docs/en/llm-gateway), [Claude Code
environment reference](https://code.claude.com/docs/en/env-vars), and [MindsHub
Inference service contract](https://mindshub.ai/inference).

## Codex CLI adapter

The Codex CLI adapter requires an OpenAI Responses-compatible endpoint. Every
launch reserves and replaces the `gowild` model-provider definition through
Codex's repeated `-c` overrides, selecting the gateway display name, Responses
base URL, `responses` wire API, and the requested model. GoWild explicitly sets
`requires_openai_auth = false`, removes inherited OpenAI/Codex credentials, and
supplies gateway authentication only through a GoWild-owned child environment
variable. Existing Codex login state and user-defined provider configuration
therefore cannot redirect the selected gateway.

Bearer credentials use `model_providers.gowild.env_key`. `x-api-key` and
custom secret-bearing headers use `env_http_headers`; optional header prefixes
are applied inside the secret environment value rather than argv. Non-secret
custom headers use the provider's `http_headers` table. Unauthenticated custom
providers omit all auth fields. Fresh sessions and `codex resume <session-id>`
receive the same provider configuration and secret environment.

The adapter contract follows the current [Codex configuration
reference](https://developers.openai.com/codex/config-reference), [Codex CLI
reference](https://developers.openai.com/codex/cli/reference), and [MindsHub
Inference service contract](https://mindshub.ai/inference).
