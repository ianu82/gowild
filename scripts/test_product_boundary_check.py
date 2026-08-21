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

    def test_retained_installers_and_crate_publish_fail_closed(self) -> None:
        self.assertEqual(boundary.check_install_boundaries(), [])

    def test_crate_package_retains_provenance_and_excludes_bytecode(self) -> None:
        manifest = (boundary.REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8")
        self.assertIn('"PROVENANCE.md"', manifest)
        self.assertIn('"!src/**/__pycache__/**"', manifest)
        self.assertIn('"!src/**/*.pyc"', manifest)

    def test_go_wild_brand_assets_replace_imported_user_visible_art(self) -> None:
        self.assertEqual(boundary.check_brand_assets(), [])


if __name__ == "__main__":
    unittest.main()
