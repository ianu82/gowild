# gowild task runner

# Run tests
test:
    cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_changelog scripts.test_config_reference_check scripts.test_docs_translation_parity scripts.test_hermes_integration_asset scripts.test_package_windows_conpty scripts.test_preview scripts.test_unix_installer scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty
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
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_changelog scripts.test_config_reference_check scripts.test_docs_translation_parity scripts.test_hermes_integration_asset scripts.test_package_windows_conpty scripts.test_preview scripts.test_unix_installer scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty
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

# Build the website and documentation
website-build:
    cd website && bun install --frozen-lockfile && bun run build

# Test bundled agent integration assets
integration-assets-test:
    bun test src/integration/assets/gowild-agent-state.test.ts
    bun test src/integration/assets/opencode/gowild-agent-state.test.ts
    bun test src/integration/assets/opencode/gowild-tui-session.test.ts

# Run plugin marketplace Worker tests
plugin-marketplace-test:
    cd workers/plugin-marketplace && bun install --frozen-lockfile && bun test

# Build the vendored libghostty-vt source dist
build-libghostty-vt:
    scripts/build_vendored_libghostty_vt.sh

# Check that release docs and changelog have been finalized from docs/next before release
release-docs-check:
    python3 scripts/agent_detection_manifest_check.py --require-website
    python3 scripts/config_reference_check.py
    node website/scripts/docs-versions.mjs check
    node website/scripts/docs-preview.mjs check
    @test -f docs/next/README.md
    @test -f docs/next/README.zh-CN.md
    @if ! diff -u CHANGELOG.md docs/next/CHANGELOG.md; then \
        echo "error: CHANGELOG.md differs from docs/next/CHANGELOG.md; finalize release notes before releasing"; \
        exit 1; \
    fi
    @for file in CONFIGURATION.md INTEGRATIONS.md SOCKET_API.md; do \
        if [ -e "$file" ]; then \
            echo "error: $file was replaced by website docs; remove the root copy"; \
            exit 1; \
        fi; \
    done
    @test -d docs/next/website/src/content/docs
    @for file in docs/next/website/src/content/docs/*.mdx; do \
        for locale in ja zh-cn; do \
            translated="docs/next/website/src/content/docs/$locale/$(basename "$file")"; \
            if [ ! -f "$translated" ]; then \
                echo "error: $translated is missing; translate next docs before releasing"; \
                exit 1; \
            fi; \
        done; \
    done
    @for file in docs/next/website/src/content/docs/ja/*.mdx docs/next/website/src/content/docs/zh-cn/*.mdx; do \
        staged="docs/next/website/src/content/docs/$(basename "$file")"; \
        if [ ! -f "$staged" ]; then \
            echo "error: $file has no matching english doc; remove the stale translation"; \
            exit 1; \
        fi; \
    done
    python3 scripts/docs_translation_parity.py --docs-root docs/next/website/src/content/docs
    just website-build
    cd website && bun run build:draft

# Validate release docs, render scaling, and end-to-end CPU before release preparation
pre-release-check:
    just release-docs-check
    just bench-render-scale
    just bench-release-smoke
    @echo "release review required: investigate material render-scaling regressions before publishing."
    @echo "release review required: update skills/gowild/SKILL.md for this stable release so it matches the current CLI, IDs, agent lifecycle semantics, and safety guidance."
    @echo "release policy: do not update skills/gowild/SKILL.md between stable releases; preview builds keep the latest stable skill."

# GoWild deliberately has no inherited release channel. These recipes remain as
# explicit safety rails until GoWild-owned signing and publishing are implemented.
release-prepare version:
    @echo "error: GoWild release preparation is disabled until a GoWild-owned release channel exists" >&2
    @exit 1

release-publish version:
    @echo "error: GoWild publishing is disabled until a GoWild-owned release channel exists" >&2
    @exit 1

release version:
    @echo "error: GoWild releases are disabled until a GoWild-owned release channel exists" >&2
    @exit 1

# Print default config
default-config:
    cargo run --release --locked -- --default-config
