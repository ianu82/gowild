# GoWild unreleased documentation

[简体中文](README.zh-CN.md)

This directory is the source of truth for GoWild behavior that has not yet been
published as a stable release.

## Product model

GoWild keeps three choices independent:

1. **Coding CLI** — Codex CLI or Claude Code initially.
2. **Gateway** — MindsHub Inference or a custom protocol-compatible endpoint.
3. **Model** — any model that the selected gateway exposes compatibly to that
   CLI.

GoWild launches the user's real installed CLI inside a persistent, server-owned
terminal. It supplies a per-launch route without changing the user's ordinary
Codex or Claude configuration. The route is saved without credentials and is
reapplied when GoWild resumes the agent.

## Current guides

- [Install and verify from source](INSTALL.md)
- [Gateway configuration and CLI routing](gateways.md)
- [Socket API schema](api/gowild-api.schema.json)
- [Unreleased changes](CHANGELOG.md)

The user-facing gateway setup is available on first run and in **settings →
gateways**. It supports the MindsHub preset, custom gateways, secure credential
replacement, protocol tests, model discovery, per-CLI defaults, and managed
launch/resume.

## Release status

There is no public GoWild binary release, hosted installer, website, or update
channel yet. Only the source-install path in this directory is supported. Every
inherited publishing path is intentionally disabled until GoWild-owned
artifacts, signing, manifests, and clean installation have been reviewed.

## Historical documentation

The nested `website/` tree and the sibling `docs/preview` and `docs/versions`
trees are frozen source-import records. They describe the imported product, not
GoWild, and must never be built or published as GoWild documentation. See
[`docs/README.md`](../README.md) and [`PROVENANCE.md`](../../PROVENANCE.md).

All GoWild work belongs only in
[`ianu82/gowild`](https://github.com/ianu82/gowild). Do not send code, issues,
pull requests, requests, or sync automation to the source-provenance project.
