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
there so keys cannot be written into normal configuration by mistake. GoWild
also rejects transport-controlled names such as `Host`, `Content-Length`, and
`Transfer-Encoding` in both custom and secret-bearing header configuration.

Credentials are addressed by an opaque reference such as `gateway:mindshub`.
GoWild first uses the operating system credential store (Keychain Services,
Windows Credential Manager, or Secret Service). On Unix, if that store is not
available, it can use a separate `credentials.json` fallback with a `0700`
directory and `0600` file. Windows fails closed because ordinary Unix mode bits
cannot establish an owner-only ACL there.

The credential type cannot be serialized, displays only `[REDACTED]` in debug
output, and zeroes its buffer when dropped. Connection diagnostics redact known
credentials and common key formats before persistence.

## TUI setup

First-run onboarding continues directly to the **gateways** settings section.
The same section opens first from the normal settings panel, without hiding the
existing theme, status, sound, toast, pane-label, or integration controls.

Select **MindsHub Inference** to:

- add or replace its API key through a fixed masked editor;
- make it the default gateway;
- run authentication, model discovery, Responses, and Messages checks in a
  background worker; and
- choose separate discovered-model defaults for Codex and Claude Code.

Choose **add custom** to configure another gateway. The form accepts a stable
gateway ID, display name, optional OpenAI Responses and Anthropic Messages base
URLs, an optional model-discovery URL, and bearer, `x-api-key`, custom secret
header, or unauthenticated access. At least one coding protocol endpoint is
required. Editing keeps the gateway ID fixed; duplicating starts an independent
custom definition and never copies the source credential. Save the definition
first, then add its API key from the gateway detail when authentication requires
one.

Custom gateway details also offer **delete**. Deletion always opens a separate
confirmation screen and defaults to removing only the gateway definition while
keeping its stored credential. Choose **delete stored credential too** before
confirming when the key should also be removed; credentials shared by another
gateway are retained automatically. Built-in presets never expose a delete
action. GoWild saves the updated gateway catalog before attempting credential
removal, so a keychain failure cannot restore a gateway that the UI reports as
deleted.

The key editor accepts typing and paste, never renders the entered value, and
clears its zeroizing buffer on save, cancel, section change, or modal close.
Connection status, per-protocol status, redacted diagnostics, and the discovered
model count are shown in the gateway detail. Model selection remains disabled
until a successful discovery has produced a selectable non-embedding model.

Keyboard navigation uses `↑`/`↓` (or `j`/`k`) for gateway rows and detail fields,
`Enter` to configure or store, `←`/`→` to change a CLI model, `Space` to set the
default, and `t` or `r` to test. In the custom form, arrows or `Tab` move between
fields, typing and paste append metadata, `Ctrl+U` clears the selected field,
and `←`/`→` changes authentication. Gateway rows, form fields, inline actions,
and footer actions are also mouse-accessible.

## Starting a managed agent

Choose **launch agent** from GoWild's global menu, or from the switcher on a
narrow terminal. The launch screen makes the complete child-process route
visible before anything starts: coding CLI, gateway, protocol, and model. Use
`↑`/`↓` to select a row, `←`/`→` to change its value, and `Enter` to launch. The
same controls are mouse-accessible. Gateway settings remain one key away with
`s`.

This is GoWild's managed launch path. It starts the CLI directly in a new tab,
without an intermediate interactive shell, and applies the adapter's argv and
secret child environment to that process. A missing model, credential,
compatible protocol, configured gateway, or CLI executable leaves the launch
screen open with an error; GoWild does not start a vendor-default fallback.

Typing `codex` or `claude` yourself in an ordinary shell remains an unmanaged
shell command and uses that CLI's own configuration. GoWild does not rewrite
arbitrary terminal input. Use **launch agent** whenever the selected gateway
must be enforced.

## Connection testing and model discovery

GoWild tests a gateway with the same configured authentication used for a real
launch. It first performs authenticated model discovery when enabled, then
sends one bounded generation probe for each advertised protocol. MindsHub is
tested with `GET /v1/models`, `POST /v1/responses`, and `POST /v1/messages`.
Redirects are disabled so a credential cannot be forwarded to another host,
responses are capped at 2 MiB, and remote endpoints must use HTTPS. Plain HTTP
is accepted only for a loopback gateway such as `localhost`.

If authenticated discovery fails, GoWild stops immediately and does not send
generation probes. A successful catalog refresh replaces the cached model
metadata and records whether each model is enabled, whether it is an embedding
model, and any advertised reasoning-effort levels. Protocol probes never select
disabled or embedding models. The saved connection report distinguishes full,
partial, and failed checks and contains only redacted diagnostics.

## Launch resolution

Every coding CLI declares the protocol it needs. GoWild resolves the selected
gateway before invoking the adapter and refuses to launch when that gateway
does not advertise a matching endpoint. Fresh launches persist their non-secret
CLI, gateway, and model route for future resume planning. Until a restored
session has been replanned through that route, GoWild suppresses the legacy raw
vendor resume command and opens a shell instead of risking a proprietary
fallback.

The managed launch screen starts from the saved gateway and per-CLI model
defaults, then records any choices there as explicit per-launch values. Parent
process environment variables cannot silently replace the route shown in the
screen. The resolved credential is passed to an adapter as a secret value,
rejected if the adapter places it in argv or an ordinary environment value, and
exposed only to the spawned child process. Launch specifications, pane
environment diagnostics, and the structured route log redact all environment
values.

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
