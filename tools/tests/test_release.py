import importlib.util
from pathlib import Path
import tempfile
import unittest


TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("release", TOOLS / "release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

CHANGELOG = """# Changelog

Intro text.

## [Unreleased]

- Pending change.

## [0.2.0] - 2026-12-01

### Fixed

- A fix.

## [0.1.0] - 2026-11-20

## [0.0.1] - 2026-01-01

- First.

[Unreleased]: https://example.invalid/compare/v0.2.0...HEAD
[0.2.0]: https://example.invalid/releases/v0.2.0
"""


class ChangelogTest(unittest.TestCase):
    def test_returns_only_the_requested_section(self):
        self.assertEqual(
            release.changelog_section(CHANGELOG, "0.2.0"), "### Fixed\n\n- A fix."
        )

    def test_link_definitions_are_not_part_of_the_last_section(self):
        self.assertEqual(release.changelog_section(CHANGELOG, "0.0.1"), "- First.")

    def test_missing_or_empty_section_fails(self):
        for version in ("0.3.0", "0.1.0"):
            with self.assertRaises(release.ReleaseError):
                release.changelog_section(CHANGELOG, version)

    def test_unreleased_fallback_only_when_allowed(self):
        self.assertEqual(
            release.changelog_section(CHANGELOG, "0.3.0", allow_unreleased=True),
            "- Pending change.",
        )


class CheckTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        (self.root / "Cargo.toml").write_text(
            '[package]\nname = "parabloom"\nversion = "0.2.0"\n'
        )
        (self.root / "CHANGELOG.md").write_text(CHANGELOG)

    def tearDown(self):
        self.directory.cleanup()

    def test_matching_tag_returns_notes(self):
        self.assertIn("A fix.", release.check("v0.2.0", self.root))

    def test_tag_must_match_cargo_version(self):
        with self.assertRaisesRegex(release.ReleaseError, "does not match"):
            release.check("v0.2.1", self.root)

    def test_tag_must_be_semver_with_v_prefix(self):
        for tag in ("0.2.0", "v0.2", "v0.2.0-rc1", "release-0.2.0"):
            with self.assertRaisesRegex(release.ReleaseError, "not of the form"):
                release.check(tag, self.root)

    def test_repository_metadata_is_consistent(self):
        # The real Cargo.toml and CHANGELOG.md must always pass the dry run.
        version = release.cargo_version()
        release.check(f"v{version}", allow_unreleased=True)


if __name__ == "__main__":
    unittest.main()
