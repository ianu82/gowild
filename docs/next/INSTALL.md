# Install and verify GoWild from source

GoWild currently supports source installation only. Public binaries, hosted
install scripts, Homebrew or mise packages, self-update, and remote release
downloads are not published yet.

The retained Unix and Windows package installers fail before network access
unless release automation supplies an explicit manifest URL or a verified local
Windows package. This is intentional and must remain true until GoWild owns a
signed release channel.

## Prerequisites

- Git
- the Rust toolchain pinned in `rust-toolchain.toml`
- CMake and Ninja
- Zig 0.15.2

Codex CLI and Claude Code are optional at build time. Install the CLI you want
to use before launching a managed agent; GoWild reports a missing executable
before launch.

## User installation

From an independently cloned `ianu82/gowild` working tree:

```bash
cargo install --path . --locked
gowild --version
```

Cargo places the executable under its install root, normally
`$HOME/.cargo/bin/gowild`. The application creates only GoWild-owned config,
state, data, runtime, and credential paths. It does not inspect or migrate
Herdr-owned state.

## Clean verification

Before distributing a source snapshot, verify a fresh install root:

```bash
temporary_root="$(mktemp -d)"
cargo install --path . --locked --root "$temporary_root"
"$temporary_root/bin/gowild" --version
```

The repository automates the same check with `just source-install-test`, and a
dedicated CI job runs it from a clean checkout.

On Windows, use an empty temporary directory with `cargo install --root` and
verify `bin\\gowild.exe --version`. The Windows CI additionally builds and tests
the app-local ConPTY package.

The Nix flake provides the same `gowild` program on supported Linux and macOS
systems. CI runs `nix flake check`; no binary cache or public Nix package is
claimed.

## First launch

Start `gowild`, complete **gateway setup**, store the credential through the
masked credential editor, test the supported protocols, refresh models, and
choose a default per CLI. The managed launch screen then selects the CLI,
gateway, protocol, and model before creating its persistent pane.

## Remove a source installation

```bash
cargo uninstall gowild
```

Uninstalling the executable does not delete user sessions or credentials. Data
removal must be a separate, explicit action; never remove a broad home or config
directory recursively as part of an installer.

## Release safety

The files under `website/` are inherited packaging and site sources retained
for audit and tests. They are not public GoWild install entry points. Do not
publish them or point users at their manifest defaults until a separately
reviewed GoWild-owned release channel is enabled.
