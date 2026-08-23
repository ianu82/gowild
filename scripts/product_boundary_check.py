#!/usr/bin/env python3
"""Keep active GoWild surfaces independent from the frozen source import."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]

ACTIVE_DOCS = (
    Path("README.md"),
    Path("README.zh-CN.md"),
    Path("assets/BRAND.md"),
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
    Path("scripts/source_install_check.py"),
)

ROOT_READMES = (Path("README.md"), Path("README.zh-CN.md"))
IMPORTED_APACHE_LICENSE_SHA256 = (
    "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4"
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


def scan_root_readme_attribution(path: Path, text: str) -> list[str]:
    source_name = "her" + "dr"
    if path not in ROOT_READMES or source_name not in text.lower():
        return []
    return [
        f"{path}: source-project attribution belongs under ACKNOWLEDGEMENTS, not in a product README"
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
        relative_path = path.relative_to(repo_root)
        text = path.read_text(encoding="utf-8")
        errors.extend(scan_text(relative_path, text))
        errors.extend(scan_root_readme_attribution(relative_path, text))
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


def check_brand_assets(repo_root: Path = REPO_ROOT) -> list[str]:
    errors: list[str] = []
    logo_svg = repo_root / "assets/logo.svg"
    logo_png = repo_root / "assets/logo.png"
    if not logo_svg.is_file():
        errors.append("assets/logo.svg: GoWild logo source is missing")
    else:
        svg = logo_svg.read_text(encoding="utf-8")
        if 'aria-label="GoWild logo"' not in svg:
            errors.append("assets/logo.svg: accessible GoWild label is missing")
        for color in ("#080D18", "#0E1626", "#22D3EE"):
            if color not in svg:
                errors.append(f"assets/logo.svg: Cowork palette color {color} is missing")

    png = logo_png.read_bytes() if logo_png.is_file() else b""
    if png[:8] != b"\x89PNG\r\n\x1a\n":
        errors.append("assets/logo.png: rendered GoWild PNG is missing or invalid")
    elif len(png) < 24 or (
        int.from_bytes(png[16:20], "big"), int.from_bytes(png[20:24], "big")
    ) != (512, 512):
        errors.append("assets/logo.png: rendered GoWild PNG must remain 512x512")

    cargo_manifest = (repo_root / "Cargo.toml").read_text(encoding="utf-8")
    for asset in ("assets/BRAND.md", "assets/logo.png", "assets/logo.svg"):
        if f'"{asset}"' not in cargo_manifest:
            errors.append(f"Cargo.toml: packaged GoWild brand asset {asset} is missing")

    for retired in ("assets/og-card.png", "assets/screenshot.png"):
        if (repo_root / retired).exists():
            errors.append(f"{retired}: imported user-visible asset must remain retired")
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


def check_install_boundaries(repo_root: Path = REPO_ROOT) -> list[str]:
    errors: list[str] = []
    unix_installer = (repo_root / "website/install.sh").read_text(encoding="utf-8")
    windows_installer = (repo_root / "website/install.ps1").read_text(encoding="utf-8")
    manifest_default = "https://github.com/ianu82/gowild/" + "latest.json"
    preview_default = "https://github.com/ianu82/gowild/" + "preview.json"

    if 'MANIFEST_URL="${GOWILD_MANIFEST_URL:-}"' not in unix_installer:
        errors.append("website/install.sh: manifest URL must require explicit release input")
    if "hosted GoWild installation is disabled" not in unix_installer:
        errors.append("website/install.sh: missing fail-closed hosted-install message")
    if manifest_default in unix_installer or "gowild.dev" in unix_installer:
        errors.append("website/install.sh: contains an unowned public install default")

    if "-not $useLocalPackage -and [string]::IsNullOrWhiteSpace($ManifestUrl)" not in windows_installer:
        errors.append("website/install.ps1: manifest/local-package gate is missing")
    if "Hosted GoWild installation is disabled" not in windows_installer:
        errors.append("website/install.ps1: missing fail-closed hosted-install message")
    if manifest_default in windows_installer or preview_default in windows_installer:
        errors.append("website/install.ps1: contains an unreviewed public manifest default")

    cargo_manifest = (repo_root / "Cargo.toml").read_text(encoding="utf-8")
    if "publish = false" not in cargo_manifest:
        errors.append("Cargo.toml: crate publishing must remain disabled")
    return errors


def check_license_boundaries(repo_root: Path = REPO_ROOT) -> list[str]:
    errors: list[str] = []
    root_license = repo_root / "LICENSE"
    if (
        not root_license.is_file()
        or root_license.read_text(encoding="utf-8").strip() != "TBD"
    ):
        errors.append("LICENSE: GoWild project licence must remain TBD until selected")

    imported_license = repo_root / "ACKNOWLEDGEMENTS/LICENSES/APACHE-2.0.txt"
    if not imported_license.is_file():
        errors.append("ACKNOWLEDGEMENTS: imported Apache License 2.0 copy is missing")
    elif (
        hashlib.sha256(imported_license.read_bytes()).hexdigest()
        != IMPORTED_APACHE_LICENSE_SHA256
    ):
        errors.append("ACKNOWLEDGEMENTS: imported Apache License 2.0 copy was altered")

    cargo_manifest = (repo_root / "Cargo.toml").read_text(encoding="utf-8")
    if 'license = "Apache-2.0"' in cargo_manifest:
        errors.append("Cargo.toml: must not claim Apache-2.0 as GoWild's project licence")
    if 'license-file = "LICENSE"' not in cargo_manifest:
        errors.append("Cargo.toml: project licence status must point to LICENSE")
    for required_path in (
        "ACKNOWLEDGEMENTS/README.md",
        "ACKNOWLEDGEMENTS/LICENSES/APACHE-2.0.txt",
    ):
        if f'"{required_path}"' not in cargo_manifest:
            errors.append(f"Cargo.toml: source package must include {required_path}")
    return errors


def check(repo_root: Path = REPO_ROOT) -> list[str]:
    return [
        *check_active_surfaces(repo_root),
        *check_brand_assets(repo_root),
        *check_frozen_website(repo_root),
        *check_release_recipes(repo_root),
        *check_install_boundaries(repo_root),
        *check_license_boundaries(repo_root),
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
