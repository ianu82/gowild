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
