"""Tests for release.py, on a copy of the repo's real version files.

    python3 -m unittest discover -s app/flatpak
"""

import contextlib
import io
import re
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import release


class CheckTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        self.version, errors = release.check(release.ROOT)
        self.assertEqual(errors, [])
        notes = release.NOTES.format(version=self.version)
        for path in (release.PUBSPEC, *release.CRATES, release.CARGO_LOCK, release.METAINFO, notes):
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(release.ROOT / path, self.root / path)

    def edit(self, path, pattern, replacement):
        file = self.root / path
        text, count = re.subn(pattern, replacement, file.read_text(), count=1, flags=re.M)
        self.assertEqual(count, 1, f"{pattern!r} not found in {path}")
        file.write_text(text)

    def assertErrors(self, *fragments, tag=None):
        _, errors = release.check(self.root, tag)
        self.assertTrue(errors, "expected errors")
        for fragment in fragments:
            self.assertIn(fragment, "\n".join(errors))

    def bump(self, version):
        """Moves every declaration to `version`, with notes for it."""
        v = re.escape(self.version)
        self.edit(release.PUBSPEC, rf"^version: {v}\+", f"version: {version}+")
        for path in release.CRATES:
            self.edit(path, rf'^version = "{v}"', f'version = "{version}"')
        for name in ("rega_core", "rega_bridge"):
            self.edit(release.CARGO_LOCK, rf'^(name = "{name}"\nversion = )"{v}"', rf'\1"{version}"')
        self.edit(release.METAINFO, r"<releases>", f'<releases>\n    <release version="{version}" date="2026-11-01"/>')
        (self.root / release.NOTES.format(version=version)).write_text("Notes.\n")

    def test_repo_agrees(self):
        self.assertRegex(self.version, r"^\d+\.\d+\.\d+$")
        self.assertEqual(release.check(self.root), (self.version, []))

    def test_tag_matches(self):
        self.assertEqual(release.check(self.root, f"v{self.version}"), (self.version, []))

    def test_tag_differs(self):
        self.assertErrors(f"tag v9.9.9 doesn't match the app's version {self.version}", tag="v9.9.9")

    def test_tag_without_v(self):
        self.assertErrors(f"tag {self.version} doesn't match", tag=self.version)

    def test_full_bump(self):
        self.bump("0.2.0")
        self.assertEqual(release.check(self.root, "v0.2.0"), ("0.2.0", []))

    def test_pubspec_alone(self):
        self.edit(release.PUBSPEC, r"^version: \S+", "version: 0.2.0+2")
        self.assertErrors("differs between files", f"{release.PUBSPEC}: 0.2.0",
                          f"{release.CRATES[0]}: {self.version}")

    def test_pubspec_build_number_ignored(self):
        self.edit(release.PUBSPEC, r"^version: (\S+)\+\d+", r"version: \1+7")
        self.assertEqual(release.check(self.root), (self.version, []))

    def test_crate_alone(self):
        self.edit(release.CRATES[1], r'^version = "[^"]*"', 'version = "0.2.0"')
        self.assertErrors(f"{release.CRATES[1]}: 0.2.0")

    def test_cargo_lock_stale(self):
        self.edit(release.CARGO_LOCK, r'^(name = "rega_core"\nversion = )"[^"]*"', r'\g<1>"0.2.0"')
        self.assertErrors(f"{release.CARGO_LOCK} (rega_core): 0.2.0")

    def test_metainfo_without_new_release(self):
        self.bump("0.2.0")
        self.edit(release.METAINFO, r'^\s*<release version="0.2.0"[^>]*/>\n', "")
        self.assertErrors(f"{release.METAINFO} (newest release): {self.version}")

    def test_metainfo_without_releases(self):
        self.edit(release.METAINFO, r"<releases>(?:.|\n)*</releases>", "")
        self.assertErrors(f"{release.METAINFO} (newest release) declares no version")

    def test_pubspec_without_version(self):
        self.edit(release.PUBSPEC, r"^version: .*\n", "")
        self.assertErrors(f"{release.PUBSPEC} declares no version")

    def test_notes_missing(self):
        self.bump("0.2.0")
        (self.root / release.NOTES.format(version="0.2.0")).unlink()
        self.assertErrors("docs/releases/0.2.0.md is missing or empty")

    def test_notes_empty(self):
        (self.root / release.NOTES.format(version=self.version)).write_text("\n")
        self.assertErrors(f"docs/releases/{self.version}.md is missing or empty")

    def test_not_semver(self):
        self.bump("0.2.0-rc1")
        self.assertErrors("version 0.2.0-rc1 isn't MAJOR.MINOR.PATCH")


class MainTest(unittest.TestCase):
    def run_main(self, *argv):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = release.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def test_prints_github_outputs(self):
        version, _ = release.check(release.ROOT)
        code, out, err = self.run_main("check", "--tag", f"v{version}")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, f"version={version}\nnotes=docs/releases/{version}.md\n")

    def test_fails_without_output(self):
        with mock.patch.object(release, "check", return_value=(None, ["bad"])):
            code, out, err = self.run_main("check")
        self.assertEqual((code, out, err), (1, "", "error: bad\n"))


if __name__ == "__main__":
    unittest.main()
