from __future__ import annotations

import unittest
from pathlib import Path

from scripts import product_boundary_check as boundary


class ProductBoundaryCheckTests(unittest.TestCase):
    def test_repository_active_surfaces_and_release_guards_pass(self) -> None:
        self.assertEqual(boundary.check(), [])

    def test_active_text_rejects_source_endpoint_and_install_markers(self) -> None:
        examples = (
            "visit https://herdr.dev/docs",
            "clone https://github.com/herdrdev/herdr",
            "brew install herdr",
            "mise use -g herdr",
        )
        for example in examples:
            with self.subTest(example=example):
                self.assertEqual(len(boundary.scan_text(Path("README.md"), example)), 1)

    def test_normal_attribution_without_source_endpoint_is_allowed(self) -> None:
        self.assertEqual(
            boundary.scan_text(
                Path("README.md"),
                "Herdr is historical source provenance and not a collaboration target.",
            ),
            [],
        )

    def test_source_attribution_is_confined_to_acknowledgements(self) -> None:
        self.assertEqual(
            len(boundary.scan_root_readme_attribution(Path("README.md"), "Herdr")),
            1,
        )
        self.assertEqual(
            boundary.scan_root_readme_attribution(
                Path("ACKNOWLEDGEMENTS/README.md"), "Herdr"
            ),
            [],
        )

    def test_owned_release_and_checksum_install_boundaries(self) -> None:
        self.assertEqual(boundary.check_install_boundaries(), [])

    def test_release_workflow_stages_before_publishing(self) -> None:
        self.assertEqual(boundary.check_release_recipes(), [])

    def test_release_workflow_allows_unsigned_mac_builds_without_keychain(self) -> None:
        workflow = (
            boundary.REPO_ROOT / ".github/workflows/binary-release.yml"
        ).read_text(encoding="utf-8")
        self.assertNotIn("APPLE_DEVELOPER_ID_CERTIFICATE_P12_BASE64", workflow)
        self.assertNotIn("APPLE_DEVELOPER_ID_SIGNING_IDENTITY", workflow)
        self.assertIn("Verify macOS credential isolation", workflow)
        self.assertIn("still contains keyring", workflow)
        self.assertIn("io.mindshub.gowild.gateway", workflow)

    def test_project_license_is_tbd_and_imported_license_is_retained(self) -> None:
        self.assertEqual(boundary.check_license_boundaries(), [])

    def test_go_wild_brand_assets_replace_imported_user_visible_art(self) -> None:
        self.assertEqual(boundary.check_brand_assets(), [])


if __name__ == "__main__":
    unittest.main()
