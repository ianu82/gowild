#!/usr/bin/env python3
"""Build the checksum-pinned manifest consumed by GoWild installers."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path


ASSET_NAMES = {
    "linux-x86_64": "gowild-linux-x86_64",
    "linux-aarch64": "gowild-linux-aarch64",
    "macos-x86_64": "gowild-macos-x86_64",
    "macos-aarch64": "gowild-macos-aarch64",
    "windows-x86_64": "gowild-windows-x86_64.zip",
}
VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def build_manifest(
    assets_dir: Path,
    version: str,
    repository: str,
    protocol: int,
    notes: str,
) -> dict[str, object]:
    if VERSION.fullmatch(version) is None:
        raise ValueError(f"invalid release version: {version}")
    if REPOSITORY.fullmatch(repository) is None:
        raise ValueError(f"invalid GitHub repository: {repository}")
    if protocol < 1:
        raise ValueError(f"invalid protocol version: {protocol}")
    if not notes.strip():
        raise ValueError("release notes must not be empty")

    assets: dict[str, str] = {}
    checksums: dict[str, str] = {}
    for target, name in ASSET_NAMES.items():
        path = assets_dir / name
        if not path.is_file() or path.is_symlink():
            raise ValueError(f"missing regular release asset: {name}")
        assets[target] = (
            f"https://github.com/{repository}/releases/download/v{version}/{name}"
        )
        checksums[target] = sha256(path)

    release_metadata = {
        "notes": notes.strip(),
        "protocol": protocol,
        "assets": assets,
        "sha256": checksums,
        "announcement": None,
    }
    return {
        "schema_version": 1,
        "version": version,
        "protocol": protocol,
        "notes": notes.strip(),
        "assets": assets,
        "sha256": checksums,
        "announcement": None,
        "releases": {version: release_metadata},
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--assets-dir", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repository", default="ianu82/gowild")
    parser.add_argument("--protocol", type=int, required=True)
    parser.add_argument("--notes", default="GoWild release")
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    manifest = build_manifest(
        arguments.assets_dir,
        arguments.version,
        arguments.repository,
        arguments.protocol,
        arguments.notes,
    )
    arguments.output.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
