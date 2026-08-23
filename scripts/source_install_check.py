#!/usr/bin/env python3
"""Install GoWild into an empty root and execute the installed artifact."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
TIMEOUT_SECONDS = 15 * 60


def expected_version() -> str:
    with (REPO_ROOT / "Cargo.toml").open("rb") as manifest_file:
        manifest = tomllib.load(manifest_file)
    return f"gowild {manifest['package']['version']}"


def run() -> None:
    with tempfile.TemporaryDirectory(prefix="gowild-source-install-") as temporary:
        root = Path(temporary)
        install_root = root / "install"
        environment = os.environ.copy()
        environment["CARGO_TARGET_DIR"] = str(REPO_ROOT / "target/source-install-check")

        install = subprocess.run(
            [
                "cargo",
                "install",
                "--path",
                str(REPO_ROOT),
                "--locked",
                "--root",
                str(install_root),
            ],
            cwd=REPO_ROOT,
            env=environment,
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECONDS,
            check=False,
        )
        if install.returncode != 0:
            raise RuntimeError(
                "source installation failed\n"
                f"stdout:\n{install.stdout}\n"
                f"stderr:\n{install.stderr}"
            )

        binary_name = "gowild.exe" if os.name == "nt" else "gowild"
        binary = install_root / "bin" / binary_name
        installed_files = sorted(path.name for path in binary.parent.iterdir())
        if installed_files != [binary_name]:
            raise RuntimeError(
                f"clean install root contained unexpected binaries: {installed_files}"
            )

        version = subprocess.run(
            [str(binary), "--version"],
            cwd=root,
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        if version.returncode != 0:
            raise RuntimeError(
                f"installed binary exited {version.returncode}: {version.stderr}"
            )
        actual_version = version.stdout.strip()
        wanted_version = expected_version()
        if actual_version != wanted_version:
            raise RuntimeError(
                f"installed binary reported {actual_version!r}, expected {wanted_version!r}"
            )

        print(f"source install check passed: {actual_version}")


def main() -> int:
    try:
        run()
    except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
