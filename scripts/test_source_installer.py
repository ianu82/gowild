from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
INSTALLER = REPO_ROOT / "scripts" / "install-from-source.sh"
SYSTEM_COMMANDS = ("dirname", "pwd")


class SourceInstallerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="gowild-source-installer-")
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.cargo_call = self.root / "cargo-call"
        for command in SYSTEM_COMMANDS:
            executable = shutil.which(command)
            self.assertIsNotNone(executable)
            (self.bin / command).symlink_to(executable)
        for command in ("cargo", "cmake", "ninja"):
            self.write_command(command, "#!/bin/sh\nexit 0\n")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_command(self, name: str, body: str) -> Path:
        path = self.bin / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)
        return path

    def run_installer(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["/bin/sh", str(INSTALLER), *arguments],
            env={
                **os.environ,
                "PATH": str(self.bin),
                "CARGO_CALL": str(self.cargo_call),
                "ZIG": "",
            },
            capture_output=True,
            text=True,
            check=False,
        )

    def test_missing_zig_fails_before_cargo(self) -> None:
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Zig 0.15.2 is required", result.stderr)
        self.assertFalse(self.cargo_call.exists())

    def test_wrong_zig_version_fails_before_cargo(self) -> None:
        self.write_command("zig", "#!/bin/sh\necho 0.14.1\n")
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires Zig 0.15.2; found 0.14.1", result.stderr)
        self.assertFalse(self.cargo_call.exists())

    def test_valid_prerequisites_install_locked_and_forward_arguments(self) -> None:
        self.write_command("zig", "#!/bin/sh\necho 0.15.2\n")
        self.write_command(
            "cargo",
            '#!/bin/sh\nprintf "%s\\n" "$ZIG" "$@" > "$CARGO_CALL"\n',
        )
        result = self.run_installer("--root", str(self.root / "install"))
        self.assertEqual(result.returncode, 0, result.stderr)
        call = self.cargo_call.read_text(encoding="utf-8").splitlines()
        self.assertEqual(call[0], str(self.bin / "zig"))
        self.assertEqual(call[1:3], ["install", "--path"])
        self.assertEqual(call[3], str(REPO_ROOT))
        self.assertEqual(call[4:], ["--locked", "--root", str(self.root / "install")])


if __name__ == "__main__":
    unittest.main()
