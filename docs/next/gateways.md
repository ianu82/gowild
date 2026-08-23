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
