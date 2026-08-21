#!/usr/bin/env python3
"""Keep active GoWild surfaces independent from the frozen source import."""

from __future__ import annotations

import json
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]

ACTIVE_DOCS = (
    Path("README.md"),
    Path("README.zh-CN.md"),
    Path("docs/README.md"),
    Path("docs/next/README.md"),
    Path("docs/next/README.zh-CN.md"),
    Path("docs/next/INSTALL.md"),
    Path("docs/next/gateways.md"),
)

ACTIVE_CODE_ROOTS = (
    Path("src"),
    Path(".github"),
    Path("nix"),
)

ACTIVE_CODE_FILES = (
    Path("AGENTS.md"),
    Path("CONTRIBUTING.md"),
    Path("Cargo.toml"),
    Path("build.rs"),
    Path("flake.nix"),
    Path("justfile"),
)

FROZEN_WEBSITE_COMMANDS = ("dev", "test", "build", "build:draft", "preview")
DISABLED_WEBSITE_SCRIPT = "node scripts/product-boundary-disabled.mjs"


def forbidden_source_markers() -> tuple[str, ...]:
    # Assemble the names so this guard does not trigger on its own source.
    source_host = "her" + "dr.dev"
    source_repository = "github.com/" + "herdrdev/" + "herdr"
    return (
        source_host,
        source_repository,
        "brew install " + "herdr",
        "mise use -g " + "herdr",
    )


def scan_text(path: Path, text: str) -> list[str]:
    lowered = text.lower()
    return [
        f"{path}: active GoWild content contains imported endpoint or install marker {marker!r}"
        for marker in forbidden_source_markers()
        if marker in lowered
    ]


def text_files_under(root: Path) -> list[Path]:
    if not root.exists():
        return []
    return sorted(
        path
        for path in root.rglob("*")
        if path.is_file() and path.suffix.lower() in {".md", ".nix", ".rs", ".toml", ".yml", ".yaml"}
    )


def check_active_surfaces(repo_root: Path = REPO_ROOT) -> list[str]:
    errors: list[str] = []
    paths = [repo_root / path for path in ACTIVE_DOCS + ACTIVE_CODE_FILES]
    for root in ACTIVE_CODE_ROOTS:
        paths.extend(text_files_under(repo_root / root))

    for path in paths:
        if not path.is_file():
            errors.append(f"{path.relative_to(repo_root)}: required active surface is missing")
            continue
        errors.extend(
            scan_text(path.relative_to(repo_root), path.read_text(encoding="utf-8"))
        )
    return errors


def check_frozen_website(repo_root: Path = REPO_ROOT) -> list[str]:
    errors: list[str] = []
    readme = repo_root / "website/README.md"
    if not readme.is_file() or "not the GoWild website" not in readme.read_text(encoding="utf-8"):
        errors.append("website/README.md: frozen imported-site warning is missing")

    package_path = repo_root / "website/package.json"
    try:
        package = json.loads(package_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        errors.append(f"website/package.json: could not verify disabled scripts: {error}")
        return errors

    scripts = package.get("scripts")
    if not isinstance(scripts, dict):
        errors.append("website/package.json: scripts must be an object")
        return errors
    for command in FROZEN_WEBSITE_COMMANDS:
        if scripts.get(command) != DISABLED_WEBSITE_SCRIPT:
            errors.append(
                f"website/package.json: {command!r} must remain fail-closed through {DISABLED_WEBSITE_SCRIPT!r}"
            )

    disabled_script = repo_root / "website/scripts/product-boundary-disabled.mjs"
    if not disabled_script.is_file() or "process.exit(1)" not in disabled_script.read_text(encoding="utf-8"):
        errors.append("website/scripts/product-boundary-disabled.mjs: fail-closed exit is missing")
    return errors


def check_release_recipes(repo_root: Path = REPO_ROOT) -> list[str]:
    justfile = (repo_root / "justfile").read_text(encoding="utf-8")
    required_errors = (
        "GoWild has no owned website yet",
        "GoWild release documentation is disabled",
        "GoWild pre-release validation is disabled",
        "GoWild release preparation is disabled",
        "GoWild publishing is disabled",
        "GoWild releases are disabled",
    )
    return [
        f"justfile: missing fail-closed release boundary {message!r}"
        for message in required_errors
        if message not in justfile
    ]


def check(repo_root: Path = REPO_ROOT) -> list[str]:
    return [
        *check_active_surfaces(repo_root),
        *check_frozen_website(repo_root),
        *check_release_recipes(repo_root),
    ]


def main() -> int:
    errors = check()
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    print("GoWild product boundary check passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
