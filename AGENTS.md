# GoWild repository instructions

GoWild is a persistent terminal runtime for coding agents with gateway-independent
model routing. The only project repository is `ianu82/gowild`.

## Repository boundary

- `herdrdev/herdr` is historical source provenance only. Never push to it, open
  issues or pull requests there, contact its maintainers, or configure it as a
  Git remote.
- The only permitted Herdr access is the already-completed read-only source
  import recorded in `ACKNOWLEDGEMENTS/README.md`. Do not fetch or merge later Herdr changes
  without explicit authorization from the GoWild owner.
- `origin` must resolve to `https://github.com/ianu82/gowild.git` before any
  push. No other push-capable remote is allowed.
- All branches, issues, pull requests, releases, packages, and automation belong
  exclusively to `ianu82/gowild`.

## Architecture

- State is separate from runtime. `AppState` remains pure data; PTYs and async
  resources belong in runtime types.
- Rendering is pure. `compute_view()` owns geometry/state projection and
  `render()` only draws.
- Avoid god objects. Keep shared gateway data, CLI adapters, persistence,
  secret storage, and TUI presentation in separate modules.
- Platform behavior belongs in `src/platform/<os>.rs`; shared modules expose
  cross-platform contracts.
- Agent detection consumes screen snapshots and remains decoupled from parser
  and viewport state.
- Shared runtime/session facts belong in the server and JSON API path. TUI-only
  presentation state stays in the client layer.
- Reuse existing modal, settings, mouse, and keyboard patterns.

## Gateway and secret safety

- Coding CLIs and gateways are independent choices. Vendor-specific launch
  behavior belongs behind CLI adapters.
- Never place API keys in argv, pane titles, logs, crash output, diagnostics,
  screenshots, fixtures, or non-secret configuration.
- Persist credential references rather than raw credentials. Prefer native OS
  credential stores and use an owner-only file only as a documented fallback.
- Tests must prove launch arguments, persisted config, and diagnostics do not
  expose secrets.
- Inherited Herdr update infrastructure stays disabled until a separately
  reviewed GoWild-owned signed release channel exists.

## Performance

Treat rendering, PTY parsing, detection, resizing, and client frame fanout as
multiplicative paths. Avoid filesystem I/O, process inspection, aggregate
snapshot creation, and avoidable allocation inside pane-scaled loops. Profile
changes to these paths with one and at least fifteen populated panes using the
repository benchmark recipes.

## Testing

Use `just` recipes by default:

```bash
just test
just check
```

Run `just check` before publishing a PR unless a narrower check is explicitly
justified. New state behavior must be testable without real PTYs. Broad identity,
session, persistence, or protocol refactors require characterization tests and
the existing adversarial invariant helpers.

Do not edit vendored code without first reading the nearest nested `AGENTS.md`.

## Git and pull requests

- Use lowercase conventional commit subjects without emojis or AI co-author
  lines.
- Keep each PR independently reviewable and green. Stack PRs when a feature
  crosses repository boundary, data model, adapters, UI, and packaging.
- Branch names created by Codex use `codex/<description>`.
- Open draft PRs by default. Never merge without the repository owner's
  direction.
- Preserve unrelated local changes.

## Rust conventions

- Do not use `unwrap()` in production code.
- Use `tracing` for logs.
- Add dependencies only when existing dependencies cannot meet the requirement.
- Compile-gate OS-specific imports, fields, functions, implementations, and
  match arms.
- Bump the runtime protocol only for an incompatible published wire change.

## Documentation and releases

- User-facing unreleased documentation lives under `docs/next/`.
- Do not edit generated or historical published documentation as part of normal
  feature work.
- GoWild release automation must use only GoWild-owned infrastructure and must
  not be enabled until its artifacts, signing, update manifests, and clean
  installation path have been verified.
