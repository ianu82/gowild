# gowild task runner

# Run tests
test:
    cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_changelog scripts.test_config_reference_check scripts.test_docs_translation_parity scripts.test_hermes_integration_asset scripts.test_package_windows_conpty scripts.test_preview scripts.test_product_boundary_check scripts.test_release_manifest scripts.test_source_installer scripts.test_unix_installer scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty
    just ui-hot-path-architecture-test
    just integration-assets-test
    just plugin-marketplace-test

# Run one nextest filter, e.g. `just test-one codex_stale_working`
test-one filter:
    cargo nextest run --locked "{{filter}}" --status-level fail --final-status-level fail --failure-output final --success-output never

# Enforce deterministic UI hot-path architecture boundaries
ui-hot-path-architecture-test:
    python3 -m unittest scripts.test_ui_hot_path_architecture

# Run fast local lint checks
[unix]
lint:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

[script("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File")]
[windows]
lint:
    & .\scripts\windows_check.ps1 -Mode lint

# Run PR CI checks
[unix]
ci filter='all()': lint
    just product-boundary-test
    cargo nextest run --locked -E "{{filter}}" --status-level fail --final-status-level slow --failure-output final --success-output never
    just ui-hot-path-architecture-test
    just integration-assets-test
    just plugin-marketplace-test

# Run Windows target lint from Unix/macOS to catch cfg(windows) compile and clippy failures before CI
[unix]
windows-lint:
    rustup target add x86_64-pc-windows-msvc
    LIBGHOSTTY_VT_SIMD=false cargo clippy --bin gowild --locked --target x86_64-pc-windows-msvc -- -D warnings

# Check formatting + run unit tests + Windows target lint + maintenance script tests
[unix]
check: ci windows-lint
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_changelog scripts.test_config_reference_check scripts.test_docs_translation_parity scripts.test_hermes_integration_asset scripts.test_package_windows_conpty scripts.test_preview scripts.test_product_boundary_check scripts.test_release_manifest scripts.test_source_installer scripts.test_unix_installer scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty
    @echo "docs reminder: if this changes user-facing behavior, make sure the relevant release docs are updated or called out before release."

[script("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File")]
[windows]
check:
    & .\scripts\windows_check.ps1 -Mode check

# Install repo-local git hooks
install-hooks:
    git config core.hooksPath .githooks
    chmod +x .githooks/pre-commit
    chmod +x .githooks/commit-msg
    @echo "installed git hooks from .githooks"

# Build release binary
[unix]
build:
    cargo build --release --locked

[script("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File")]
[windows]
build:
    cargo build --release --locked

# Non-gating full-render scaling profile for background workspaces and active panes
bench-render-scale:
    cargo test --release --locked --bin gowild render_scale_profile -- --ignored --nocapture --test-threads=1

# ~3-5 minute CPU comparison; downloads stable unless GOWILD_PERF_BASELINE_BIN is set
bench-release-smoke:
    cargo build --release --locked
    scripts/release_perf_smoke.sh "${CARGO_TARGET_DIR:-target}/release/gowild"

# The imported website snapshot is not GoWild content and must never be built.
website-build:
    @echo "error: GoWild has no owned website yet; the inherited snapshot is frozen and unpublishable" >&2
    @exit 1

# Test bundled agent integration assets
integration-assets-test:
    bun test src/integration/assets/gowild-agent-state.test.ts
    bun test src/integration/assets/opencode/gowild-agent-state.test.ts
    bun test src/integration/assets/opencode/gowild-tui-session.test.ts

# Keep active docs, automation, and release entry points inside GoWild's boundary
product-boundary-test:
    python3 -m unittest scripts.test_product_boundary_check

# Run plugin marketplace Worker tests
plugin-marketplace-test:
    cd workers/plugin-marketplace && bun install --frozen-lockfile && bun test

# Non-gating compatibility check against locally installed upstream CLIs
[unix]
real-cli-gateway-routing-test:
    python3 scripts/real_cli_gateway_routing_check.py

# Install into an empty root and execute only the resulting GoWild binary
[unix]
source-install-test:
    python3 scripts/source_install_check.py

# Build the vendored libghostty-vt source dist
build-libghostty-vt:
    scripts/build_vendored_libghostty_vt.sh

# Check the owned release documentation and manifest tooling.
release-docs-check:
    test -f docs/next/INSTALL.md
    python3 -m unittest scripts.test_release_manifest scripts.test_unix_installer

# Release validation is non-publishing; GitHub Actions owns publication.
pre-release-check:
    cargo fmt --check
    just product-boundary-test
    python3 -m unittest scripts.test_release_manifest scripts.test_unix_installer

# Local recipes remain fail-closed so releases can only be assembled from a
# pushed version tag by the reviewed binary-release workflow.
release-prepare version:
    @echo "error: release preparation is owned by .github/workflows/binary-release.yml" >&2
    @exit 1

release-publish version:
    @echo "error: local publishing is disabled; use the reviewed version-tag workflow" >&2
    @exit 1

release version:
    @echo "error: local releases are disabled; use the reviewed version-tag workflow" >&2
    @exit 1

# Print default config
default-config:
    cargo run --release --locked -- --default-config
