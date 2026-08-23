<p align="center">
  <img src="assets/logo.svg" width="132" alt="GoWild logo">
</p>

<h1 align="center">GoWild</h1>

<p align="center"><strong>Use the coding agents you love with the inference gateway and models you choose.</strong></p>

<p align="center">
  Native Codex and Claude interfaces · Explicit model routing · Persistent multi-agent workspaces
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#why-gowild">Why GoWild</a> ·
  <a href="#using-gowild">How to use it</a> ·
  <a href="docs/next/gateways.md">Gateway guide</a>
</p>

GoWild is a persistent terminal workspace for AI coding agents. It separates
three choices that are usually bundled together:

1. **Coding interface** — start with Codex CLI or Claude Code.
2. **Inference gateway** — use MindsHub Inference or a compatible custom gateway.
3. **Model** — choose any model the gateway exposes for that CLI's protocol.

You keep the native agent experience. GoWild applies and displays the route,
stores it with the session, and restores it when you come back—without rewriting
your normal Codex or Claude configuration.

<p align="center">
  <img src="assets/managed-route.png" width="100%" alt="GoWild displaying a managed Codex route through MindsHub Inference">
</p>

```text
Codex CLI   → GoWild → OpenAI Responses    → gateway → chosen model
Claude Code → GoWild → Anthropic Messages  → gateway → chosen model
```

## Why GoWild

| You want to… | GoWild gives you… |
| --- | --- |
| Choose where inference runs | A built-in MindsHub route plus custom OpenAI Responses and Anthropic Messages gateways |
| Keep familiar agent workflows | The real, unmodified Codex CLI and Claude Code interfaces |
| Know what will happen before launch | A fail-closed route screen showing CLI, gateway, protocol, and full model ID |
| Work with several agents at once | Workspaces, tabs, panes, agent status, and a managed Codex-to-Claude handoff |
| Close the terminal without losing work | Persistent named sessions, detach/reattach, native agent resume, and SSH attachment |
| Avoid leaking credentials | OS credential storage, masked entry, redacted diagnostics, and child-only secret injection |
| Find the right model quickly | Authenticated discovery and a searchable chooser with separate defaults per CLI |
| Make the terminal feel like yours | Cowork dark, Cowork light, follow-terminal mode, mouse support, and configurable shortcuts |

Managed routes do not silently fall back to a proprietary provider. If the
gateway, credential, protocol, model, or CLI executable cannot be applied,
GoWild leaves the launch screen open with an actionable error.

## Quick start

GoWild currently ships as a source install. You need Git, the pinned Rust
toolchain, CMake, Ninja, and Zig 0.15.2. Install Codex CLI and/or Claude Code
before launching the corresponding managed agent.

```bash
git clone https://github.com/ianu82/gowild.git
cd gowild
cargo install --path . --locked
gowild --version
```

Then open a project in a named persistent session:

```bash
cd path/to/your/project
gowild --session my-project
```

First-run setup walks you through the complete path:

1. Detect installed Codex and Claude executables.
2. Connect MindsHub Inference or add a custom gateway.
3. Store the API key through GoWild's masked credential editor.
4. Verify authentication, model discovery, and the supported protocols.
5. Choose separate default models for Codex and Claude.
6. Review the full route and launch your first managed agent.

API keys never belong in repository files, normal configuration, or command
arguments. See the [installation guide](docs/next/INSTALL.md) for clean-install,
platform, Nix, and removal details.

## Using GoWild

### Launch coding agents

The guided setup offers the first launch automatically. Later, press the GoWild
prefix (`Ctrl+B` by default), then `a`, or choose **launch coding agent** from
the menu.

Select the CLI, gateway, and model. GoWild chooses the compatible protocol and
shows the complete route before `Enter` starts the agent in a managed tab.
Launch another agent to keep Codex and Claude in the same workspace while
retaining each native interface.

> Typing `codex` or `claude` into an ordinary shell is intentionally unmanaged
> and uses that CLI's own configuration. Use **launch coding agent** when the
> selected GoWild gateway must be enforced.

### Configure MindsHub or a custom gateway

Open **settings → gateways** to:

- add or replace a credential securely;
- test authentication, model discovery, Responses, and Messages;
- choose a default gateway and separate Codex/Claude models; or
- add a custom gateway supporting Responses, Messages, or both.

GoWild accepts bearer tokens, `x-api-key`, custom secret headers, and
unauthenticated loopback endpoints where the target CLI can apply them safely.
Remote gateway endpoints must use HTTPS; redirects and embedded URL credentials
are rejected.

The [gateway guide](docs/next/gateways.md) documents the security model,
compatibility bridges, custom gateway fields, and exact adapter behavior.

### Leave work running

Detach from GoWild with `Ctrl+B`, then `q`. Agents, shells, tests, and servers
continue running. Reattach later with:

```bash
gowild session attach my-project
```

List or stop named sessions explicitly:

```bash
gowild session list
gowild session stop my-project
```

Attach to a GoWild server over SSH with:

```bash
gowild --remote user@host --session my-project
```

### See what every agent is doing

The workspace sidebar keeps managed routing facts visible and distinguishes
managed agents from ordinary shell-launched CLIs. GoWild also tracks agent
states such as working, idle, done, blocked, or unknown so you can move between
tasks without polling every tab.

## Current support

| Coding CLI | Managed protocol | Built-in MindsHub route | Native resume |
| --- | --- | --- | --- |
| Codex CLI | OpenAI Responses | Yes | Yes |
| Claude Code | Anthropic Messages | Yes | Yes |

Other detected coding agents can still run inside GoWild terminals, but their
inference gateway is not managed yet.

GoWild does not yet publish stable binaries, hosted installers, or an update
channel. Source installation is the supported path while GoWild-owned release
artifacts, signing, and manifests are built and verified.

## For automation and contributors

GoWild exposes workspace, tab, pane, agent, worktree, notification, and session
commands over its local socket API. Run `gowild --skill` for the bundled agent
control guide and `gowild --help` for the CLI surface.

```bash
just test
just check
cargo run -- --help
```

- [Unreleased product documentation](docs/next/README.md)
- [Gateway architecture](docs/next/gateways.md)
- [Socket API schema](docs/next/api/gowild-api.schema.json)
- [Contributing](CONTRIBUTING.md)

## Licence

GoWild is licensed under the [Apache License 2.0](LICENSE). Historical source
attribution and retained acknowledgements are recorded under
[ACKNOWLEDGEMENTS](ACKNOWLEDGEMENTS/README.md).
