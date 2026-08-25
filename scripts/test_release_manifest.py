from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from scripts import release_manifest


class ReleaseManifestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="gowild-release-manifest-")
        self.assets = Path(self.temporary.name)
        for target, name in release_manifest.ASSET_NAMES.items():
            (self.assets / name).write_bytes(f"{target}\n".encode())

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_manifest_pins_every_owned_asset(self) -> None:
        manifest = release_manifest.build_manifest(
            self.assets, "0.1.0", "ianu82/gowild", 20, "Initial release"
        )
        self.assertEqual(manifest["schema_version"], 1)
        self.assertEqual(manifest["version"], "0.1.0")
        self.assertEqual(manifest["protocol"], 20)
        self.assertEqual(manifest["notes"], "Initial release")
        self.assertEqual(set(manifest["assets"]), set(release_manifest.ASSET_NAMES))
        self.assertEqual(set(manifest["sha256"]), set(release_manifest.ASSET_NAMES))
        self.assertEqual(manifest["releases"]["0.1.0"]["assets"], manifest["assets"])
        self.assertEqual(manifest["releases"]["0.1.0"]["sha256"], manifest["sha256"])
        for target, name in release_manifest.ASSET_NAMES.items():
            self.assertEqual(
                manifest["assets"][target],
                f"https://github.com/ianu82/gowild/releases/download/v0.1.0/{name}",
            )
            self.assertRegex(manifest["sha256"][target], r"^[0-9a-f]{64}$")

    def test_missing_asset_fails_closed(self) -> None:
        (self.assets / "gowild-macos-aarch64").unlink()
        with self.assertRaisesRegex(ValueError, "missing regular release asset"):
            release_manifest.build_manifest(
                self.assets, "0.1.0", "ianu82/gowild", 20, "Initial release"
            )

    def test_invalid_release_identity_is_rejected(self) -> None:
        for version, repository in (
            ("0.1", "ianu82/gowild"),
            ("0.1.0/escape", "ianu82/gowild"),
            ("0.1.0", "https://github.com/ianu82/gowild"),
        ):
            with self.subTest(version=version, repository=repository):
                with self.assertRaisesRegex(ValueError, "invalid"):
                    release_manifest.build_manifest(
                        self.assets, version, repository, 20, "Initial release"
                    )

    def test_invalid_protocol_and_empty_notes_are_rejected(self) -> None:
        for protocol, notes in ((0, "Initial release"), (20, "  ")):
            with self.subTest(protocol=protocol, notes=notes):
                with self.assertRaisesRegex(ValueError, "invalid|must not be empty"):
                    release_manifest.build_manifest(
                        self.assets, "0.1.0", "ianu82/gowild", protocol, notes
                    )


if __name__ == "__main__":
    unittest.main()
