"""Tests for sources.py, on a copy of the repo's real files.

    python3 -m unittest discover -s app/flatpak
"""

import json
import re
import shutil
import tempfile
import unittest
from pathlib import Path

import sources


class SourcesTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        for path in (sources.MANIFEST, sources.FLUTTER_MODULE, sources.RUST_MODULE, sources.WORKFLOW,
                     sources.CARGO_LOCK, *sources.PUB_LOCKS):
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(sources.ROOT / path, self.root / path)

    def edit(self, path, pattern, replacement):
        file = self.root / path
        text, count = re.subn(pattern, replacement, file.read_text(), count=1, flags=re.M)
        self.assertEqual(count, 1, f"{pattern!r} not found in {path}")
        file.write_text(text)

    def locked(self, package, lock=sources.PUB_LOCKS[0]):
        return sources.parse_pub_lock(sources.read(self.root, lock), lock)[package]["version"]

    def assertDrift(self, *fragments):
        errors = "\n".join(sources.check(self.root))
        for fragment in fragments:
            self.assertIn(fragment, errors)

    def test_repo_matches(self):
        self.assertEqual(sources.check(sources.ROOT), [])

    def test_flutter_bumped(self):
        flutter = sources.workflow_version(self.root, "FLUTTER_VERSION")
        self.edit(sources.WORKFLOW, r"^(\s*FLUTTER_VERSION:) \S+", r"\1 9.9.9")
        self.assertDrift(f"flutter.yml has Flutter {flutter}, but CI builds with 9.9.9", "sources.py update")

    def test_rust_bumped(self):
        rust = sources.workflow_version(self.root, "RUST_VERSION")
        self.edit(sources.WORKFLOW, r"^(\s*RUST_VERSION:) \S+", r"\1 9.9.9")
        self.assertDrift(f"rust.yml has Rust {rust}, but CI builds with 9.9.9", "sources.py update")

    def test_rust_components_disagree(self):
        rust = sources.workflow_version(self.root, "RUST_VERSION")
        self.edit(sources.RUST_MODULE, r"/cargo-[0-9.]+-", "/cargo-9.9.9-")
        self.assertDrift(f"rust.yml has Rust {rust}, 9.9.9, but CI builds with {rust}")

    def test_module_left_out_of_manifest(self):
        self.edit(sources.MANIFEST, r"^\s*- rust\.yml\n", "")
        self.assertDrift("doesn't include rust.yml in its modules")
        self.assertNotIn("after FLUTTER_VERSION", "\n".join(sources.check(self.root)))

    def test_flutter_downloads_another_engine(self):
        self.edit(sources.FLUTTER_MODULE, r"(/flutter_infra_release/flutter/)[0-9a-f]{40}(/linux-x64/)",
                  r"\g<1>" + "a" * 40 + r"\2")
        self.assertDrift(f"downloads engine {'a' * 40}, ")

    def test_flutter_downloads_other_fonts(self):
        self.edit(sources.FLUTTER_MODULE, r"(/flutter/fonts/)[0-9a-f]{40}/", r"\g<1>" + "a" * 40 + "/")
        self.assertDrift("doesn't download the material_fonts it checks for")

    def test_flutter_version_check_left_out(self):
        self.edit(sources.FLUTTER_MODULE, r"^\s*- grep -qxF \S+ flutter/bin/internal/engine\.version\n", "")
        self.assertDrift("doesn't check the versions in Flutter's bin/internal")

    def test_generated_sources_left_out_of_manifest(self):
        self.edit(sources.MANIFEST, r"^\s*- generated/cargo-sources\.json\n", "")
        self.assertDrift("doesn't use app/flatpak/generated/cargo-sources.json")

    def test_source_without_hash(self):
        self.edit(sources.FLUTTER_MODULE, r"^(\s*url: \"https://pub\.dev/api/archives/args-.*\n)\s*sha256: \S+\n",
                  r"\1")
        self.assertDrift("flutter.yml has https://pub.dev/api/archives/args-")

    def test_git_without_commit(self):
        self.edit(sources.FLUTTER_MODULE, r"^\s*commit: \S+\n", "")
        self.assertDrift("flutter.yml has https://github.com/flutter/flutter.git with no sha256")

    def test_pub_git_dependency(self):
        self.edit(sources.PUB_LOCKS[0], r"^  args:\n(?:    .*\n)+",
                  '  args:\n    dependency: transitive\n    description:\n      path: "."\n'
                  '      ref: main\n      resolved-ref: "' + "a" * 40 + '"\n'
                  '      url: "https://github.com/dart-lang/args.git"\n    source: git\n    version: "2.7.0"\n')
        self.assertDrift("app/pubspec.lock: args comes from git", "teach sources.py")

    def test_pub_package_without_hash(self):
        self.edit(sources.PUB_LOCKS[0], r"^(  args:\n(?:    .*\n)*?)      sha256: \S+\n", r"\1")
        self.assertDrift(f"app/pubspec.lock: args {self.locked('args')} has no sha256")

    def test_crate_from_git(self):
        self.edit(sources.CARGO_LOCK, r'^(name = "base64"\nversion = "[^"]+"\n)source = .*\nchecksum = .*\n',
                  r'\1source = "git+https://github.com/marshallpierce/rust-base64#abc"' + "\n")
        self.assertDrift("app/Cargo.lock: base64 ", "comes from git+https://github.com/marshallpierce")

    def test_write(self):
        self.assertEqual(sources.write(self.root), [])
        cargo = json.loads((self.root / sources.CARGO_SOURCES).read_text())
        pub = json.loads((self.root / sources.PUB_SOURCES).read_text())
        for source in cargo + pub:
            if source["type"] == "archive":
                self.assertRegex(source["sha256"], r"^[0-9a-f]{64}$")
                self.assertTrue(source["url"].startswith("https://"))
        crates = [s for s in cargo if s["type"] == "archive"]
        self.assertEqual(len(crates), (self.root / sources.CARGO_LOCK).read_text().count("\nchecksum = "))
        self.assertEqual(cargo[-1]["dest-filename"], "config.toml")
        # Packages both locks have are there once.
        dests = [s["dest"] for s in pub if s["type"] == "archive"]
        self.assertEqual(len(dests), len(set(dests)))
        # Both locks have args, at different versions (cargokit's is older).
        for lock in sources.PUB_LOCKS:
            self.assertIn(f"{sources.PUB_CACHE}/hosted/pub.dev/args-{self.locked('args', lock)}", dests)
        self.assertEqual(len({self.locked("args", lock) for lock in sources.PUB_LOCKS}), 2)

    def test_write_refuses_what_it_cant_source(self):
        self.edit(sources.CARGO_LOCK, r'^(name = "base64"\nversion = "[^"]+"\nsource = .*\n)checksum = .*\n', r"\1")
        self.assertNotEqual(sources.write(self.root), [])
        self.assertFalse((self.root / sources.CARGO_SOURCES).exists())


class ParseTest(unittest.TestCase):
    def test_pub_lock(self):
        lock = sources.parse_pub_lock(
            "# Generated by pub\npackages:\n  args:\n    dependency: transitive\n    description:\n"
            '      name: args\n      sha256: "' + "a" * 64 + '"\n      url: "https://pub.dev"\n'
            '    source: hosted\n    version: "2.7.0"\n  flutter:\n    dependency: "direct main"\n'
            '    description: flutter\n    source: sdk\n    version: "0.0.0"\nsdks:\n  dart: ">=3.9.0 <4.0.0"\n',
            "pubspec.lock")
        self.assertEqual(lock["args"]["description"]["sha256"], "a" * 64)
        self.assertEqual(lock["args"]["version"], "2.7.0")
        self.assertEqual(lock["flutter"]["description"], "flutter")
        self.assertNotIn("dart", lock)

    def test_pubspec_pins(self):
        pins = sources.pubspec_pins(
            "name: flutter_tools\nenvironment:\n  sdk: ^3.11.0-0\n\ndependencies:\n  args: 2.7.0\n"
            "  multicast_dns: 0.3.3+1 # comment\n\ndev_dependencies:\n  test: 1.31.1\n\ndartdoc:\n  nodoc: true\n",
            "pubspec.yaml")
        self.assertEqual(pins, {"args": "2.7.0", "multicast_dns": "0.3.3+1", "test": "1.31.1"})

    def test_patch_misses(self):
        patch = "Why.\n\n--- a/x.sh\n+++ b/x.sh\n@@ -1,3 +1,3 @@\n keep\n-  old line\n+  new line\n"
        self.assertEqual(sources.patch_misses(patch, "keep\n  old line\nmore\n"), [])
        self.assertEqual(sources.patch_misses(patch, "keep\n  changed line\n"), ["  old line"])

    def test_pubspec_pins_refuses_ranges(self):
        with self.assertRaises(SystemExit):
            sources.pubspec_pins("dependencies:\n  args: ^2.7.0\n", "pubspec.yaml")


if __name__ == "__main__":
    unittest.main()
