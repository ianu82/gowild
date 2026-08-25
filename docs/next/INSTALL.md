# Install GoWild

## macOS and Linux

The GoWild installer downloads the binary for the current architecture and
verifies its SHA-256 checksum against the manifest attached to the same GitHub
release:

```bash
curl -fsSL https://raw.githubusercontent.com/ianu82/gowild/main/website/install.sh | sh
```

It prefers a writable user-owned directory already on `PATH`; otherwise it uses
`$HOME/.local/bin` and prints the exact path to launch. Override the destination
with `GOWILD_INSTALL_DIR` when required.

## Windows

Run the GoWild PowerShell installer in a non-administrator PowerShell window:

```powershell
irm https://raw.githubusercontent.com/ianu82/gowild/main/website/install.ps1 | iex
```

The Windows release is a checksum-verified bundle containing GoWild and its
app-local ConPTY runtime. The installer uses versioned directories and updates
the user `PATH` without requiring administrator access.

GoWild release binaries are not yet code-signed or notarized. The installer
still fails closed on a missing asset, malformed manifest, missing checksum,
checksum mismatch, or incomplete Windows runtime bundle.

On macOS, gateway credentials are stored in GoWild's owner-only config directory
instead of Keychain, so installing or updating the unsigned CLI does not create
repeated Keychain authorization prompts. Existing Keychain entries from earlier
builds are left untouched; enter the gateway key once in the updated GoWild
setup to create the owner-only credential file.

## Build from source

### Prerequisites

- Git
- the Rust toolchain pinned in `rust-toolchain.toml`
- CMake and Ninja
- Zig 0.15.2

Codex CLI and Claude Code are optional at build time. Install the CLI you want
to use before launching a managed agent; GoWild reports a missing executable
before launch.

From an independently cloned `ianu82/gowild` working tree:

```bash
./scripts/install-from-source.sh
gowild --version
```

The installer validates Rust, CMake, Ninja, and exactly Zig 0.15.2 before Cargo
starts compiling. On macOS, install the native prerequisites with
`brew install cmake ninja zig@0.15`; the installer automatically locates the
versioned Homebrew Zig formula even when it is not on `PATH`.

Cargo places the executable under its install root, normally
`$HOME/.cargo/bin/gowild`. The application creates only GoWild-owned config,
state, data, runtime, and credential paths. It does not inspect or migrate
state owned by the imported source application.

### Clean source verification

Before distributing a source snapshot, verify a fresh install root:

```bash
temporary_root="$(mktemp -d)"
./scripts/install-from-source.sh --root "$temporary_root"
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

## Remove GoWild

For a direct Unix install, remove the exact `gowild` path printed by the
installer. On Windows, remove the GoWild entry from the user installation
directory and user `PATH`.

For a Cargo source installation:

```bash
cargo uninstall gowild
```

Uninstalling the executable does not delete user sessions or credentials. Data
removal must be a separate, explicit action; never remove a broad home or config
directory recursively as part of an installer.

## Release verification

Every GitHub release is assembled as a complete set before publication: four
Unix binaries, the Windows ConPTY bundle, `latest.json`, and
`SHA256SUMS`. Tag and Cargo versions must match, every package is built from the
tag, and publication remains a draft until the workflow verifies the complete
asset set.
