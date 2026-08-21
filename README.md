# GoWild

GoWild is a persistent terminal runtime for coding agents where the CLI, LLM
gateway, and model are independent choices.

It starts from the battle-tested persistent PTY runtime recorded in
[PROVENANCE.md](PROVENANCE.md) and is evolving into a MindsHub Cowork-branded
product that can launch installed coding CLIs against protocol-compatible LLM
gateways.

## Product direction

- Persistent workspaces, tabs, panes, sessions, and remote reattachment.
- Agent working, blocked, and idle state detection.
- Native Codex CLI and Claude Code interfaces.
- OpenAI Responses-compatible gateways for Codex.
- Anthropic Messages-compatible gateways for Claude Code.
- MindsHub Inference as the first preset, with custom gateways supported by the
  same adapter architecture.
- Secure credential storage and per-CLI model selection in the TUI.

Gateway configuration is under active development. The repository does not yet
publish stable binaries, installers, or an update channel.

## Development

GoWild is a Rust application and retains the inherited `just` workflows:

```bash
just test
just check
cargo run -- --help
```

The executable and all new runtime state use the `gowild` identity. GoWild does
not read or migrate Herdr configuration or session state.

## Repository boundary

All GoWild work happens in
[`ianu82/gowild`](https://github.com/ianu82/gowild). Do not submit GoWild code,
issues, or requests to the Herdr project. See [PROVENANCE.md](PROVENANCE.md) for
the read-only source attribution and exact imported baseline.

## Licence

Apache License 2.0. See [LICENSE](LICENSE).
