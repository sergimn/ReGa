"""Tests for rust_notices.py, on made-up crates; no cargo needed.

    python3 -m unittest discover -s app/flatpak
"""

import hashlib
import json
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import rust_notices
from rust_notices import Crate

MIT = b"MIT License\n\nCopyright (c) 2024 Someone\n\nPermission is hereby granted...\n"
APACHE = b"                                 Apache License\n                           Version 2.0\n"


class AcceptedTest(unittest.TestCase):
    def test_permissive(self):
        for expression in ("MIT", "MIT OR Apache-2.0", "Apache-2.0 / MIT", "MIT/Apache-2.0",
                           "(MIT OR Apache-2.0) AND Unicode-3.0", "Apache-2.0 WITH LLVM-exception",
                           "ISC AND (Apache-2.0 OR ISC) AND (Apache-2.0 OR ISC OR MIT-0)", "mit or Apache-2.0"):
            with self.subTest(expression):
                self.assertTrue(rust_notices.accepted(expression))

    def test_one_side_of_or_is_enough(self):
        self.assertTrue(rust_notices.accepted("GPL-3.0-only OR MIT"))
        self.assertTrue(rust_notices.accepted("MIT OR GPL-3.0-only"))

    def test_both_sides_of_and(self):
        self.assertFalse(rust_notices.accepted("GPL-3.0-only"))
        self.assertFalse(rust_notices.accepted("MIT AND GPL-3.0-only"))
        self.assertFalse(rust_notices.accepted("(MIT OR Apache-2.0) AND MPL-2.0"))
        self.assertFalse(rust_notices.accepted("LGPL-2.1-or-later WITH LLVM-exception"))

    def test_not_an_expression(self):
        for expression in ("", "MIT OR", "(MIT", "MIT)", "MIT Apache-2.0", "AND MIT", "MIT OR / Apache-2.0"):
            with self.subTest(expression), self.assertRaises(ValueError):
                rust_notices.accepted(expression)


class Crates(unittest.TestCase):
    """A made-up repo root and cargo registry to put crates in."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.root = self.tmp / "repo"
        (self.root / rust_notices.TEXTS).mkdir(parents=True)
        for table in ("UNSHIPPED", "LICENSE_FILE_ONLY"):
            patcher = mock.patch.dict(getattr(rust_notices, table), clear=True)
            patcher.start()
            self.addCleanup(patcher.stop)

    def crate(self, name, license="MIT", files=None, license_file=None, version="1.0.0"):
        path = self.tmp / "registry" / f"{name}-{version}"
        path.mkdir(parents=True)
        for file, data in (files or {}).items():
            (path / file).parent.mkdir(parents=True, exist_ok=True)
            (path / file).write_bytes(data)
        return Crate(name, version, license, license_file, path, f"https://example.com/{name}")

    def check_in(self, name, license, files):
        rust_notices.UNSHIPPED[name] = license
        for file, data in files.items():
            (self.root / rust_notices.TEXTS / name).mkdir(exist_ok=True)
            (self.root / rust_notices.TEXTS / name / file).write_bytes(data)

    def collect(self, *crates):
        return rust_notices.collect(self.root, list(crates))


class LicenseFilesTest(Crates):
    def test_finds_licences_not_code(self):
        crate = self.crate("c", files={
            "LICENSE-MIT": MIT, "LICENSE-APACHE.md": APACHE, "COPYRIGHT": b"x", "NOTICE": b"x", "UNLICENSE": b"x",
            "LICENSES/BSD-3-Clause.txt": b"x", "vendor/lib/LICENSE": b"x", "README.md": b"x",
            "src/license.rs": b"x", "src/licenses/mod.rs": b"x", "src/lib.rs": b"x", "licenses.json": b"x",
        })
        found = [p.relative_to(crate.path).as_posix() for p in rust_notices.license_files(crate.path)]
        self.assertEqual(found, ["COPYRIGHT", "LICENSE-APACHE.md", "LICENSE-MIT", "NOTICE", "UNLICENSE",
                                 "LICENSES/BSD-3-Clause.txt", "vendor/lib/LICENSE"])

    def test_license_file_with_any_name(self):
        crate = self.crate("c", files={"legal/terms.txt": b"x"})
        self.assertEqual(rust_notices.license_files(crate.path, "legal/terms.txt"), [crate.path / "legal/terms.txt"])
        self.assertEqual(rust_notices.license_files(crate.path, "./legal/../legal/terms.txt"),
                         [crate.path / "legal/terms.txt"])

    def test_license_file_missing_or_outside(self):
        crate = self.crate("c", files={"README.md": b"x"})
        (crate.path.parent / "LICENSE").write_bytes(MIT)
        self.assertEqual(rust_notices.license_files(crate.path, "../LICENSE"), [])
        self.assertEqual(rust_notices.license_files(crate.path, "LICENSE"), [])


class NormalizeTest(unittest.TestCase):
    def test_same_text_however_saved(self):
        self.assertEqual(rust_notices.normalize(b"\xef\xbb\xbf\r\n  MIT  \r\n\r\nline\r\n\r\n"), "  MIT\n\nline\n")
        self.assertEqual(rust_notices.normalize(b"MIT\nline"), "MIT\nline\n")

    def test_not_utf8(self):
        self.assertEqual(rust_notices.normalize(b"Copyright \xa9 1999"), "Copyright © 1999\n")


class CollectTest(Crates):
    def assertError(self, crates, *fragments):
        """collect() finds one problem, and says all of fragments about it."""
        _, errors, _ = self.collect(*crates)
        self.assertEqual(len(errors), 1, errors)
        for fragment in fragments:
            self.assertIn(fragment, errors[0])

    def test_crate_ships_its_texts(self):
        [entry], errors, notes = self.collect(
            self.crate("c", "MIT OR Apache-2.0", {"LICENSE-MIT": MIT, "LICENSE-APACHE": APACHE}))
        self.assertEqual((errors, notes), ([], []))
        self.assertEqual(entry.license, "MIT OR Apache-2.0")
        self.assertEqual([t.source for t in entry.texts], ["LICENSE-APACHE", "LICENSE-MIT"])
        self.assertEqual(entry.texts[1].body, MIT.decode())

    def test_licence_not_accepted(self):
        self.assertError([self.crate("c", "GPL-3.0-only", {"COPYING": b"GPL"})], "c 1.0.0 (https://example.com/c) "
                          "is under GPL-3.0-only", "add the licence to ACCEPTED")

    def test_licence_unreadable(self):
        self.assertError([self.crate("c", "MIT OR", {"LICENSE": MIT})], "can't read its licence")

    def test_no_text(self):
        self.assertError([self.crate("c", "MIT", {"README.md": b"x"})], "ships no licence text of its own",
                          ".cargo_vcs_info.json")

    def test_vendored_text_is_not_its_own(self):
        self.assertError([self.crate("c", "MIT", {"vendor/LICENSE": b"BSD"})], "ships no licence text of its own")

    def test_text_checked_in(self):
        self.check_in("c", "MIT", {"LICENSE": MIT})
        [entry], errors, notes = self.collect(self.crate("c", "MIT", {"vendor/LICENSE": b"BSD"}))
        self.assertEqual((errors, notes), ([], []))
        self.assertEqual([t.source for t in entry.texts],
                         ["app/flatpak/rust-licenses/c/LICENSE, from its repository", "vendor/LICENSE"])
        self.assertEqual(entry.texts[0].body, MIT.decode())

    def test_checked_in_text_was_for_another_licence(self):
        self.check_in("c", "MIT", {"LICENSE": MIT})
        self.assertError([self.crate("c", "Apache-2.0")], "says `Apache-2.0` now", "taken for `MIT`")

    def test_checked_in_folder_empty(self):
        rust_notices.UNSHIPPED["c"] = "MIT"
        self.assertError([self.crate("c")], "nothing in app/flatpak/rust-licenses/c/")

    def test_checked_in_text_not_needed_any_more(self):
        self.check_in("c", "MIT", {"LICENSE": MIT})
        self.check_in("gone", "MIT", {"LICENSE": MIT})
        [entry], errors, notes = self.collect(self.crate("c", "MIT", {"LICENSE": MIT}))
        self.assertEqual(errors, [])
        self.assertEqual([t.source for t in entry.texts], ["LICENSE"])
        self.assertEqual(len(notes), 2)
        self.assertIn("UNSHIPPED has gone, which isn't in the build any more", notes[0])
        self.assertIn("c 1.0.0 ships its licence text now", notes[1])

    def test_license_file_only(self):
        rust_notices.LICENSE_FILE_ONLY["c"] = ("Apache-2.0", hashlib.sha256(APACHE).hexdigest())
        [entry], errors, _ = self.collect(self.crate("c", None, {"LICENSE": APACHE}, "LICENSE"))
        self.assertEqual(errors, [])
        self.assertEqual(entry.license, "Apache-2.0")

    def test_license_file_changed(self):
        rust_notices.LICENSE_FILE_ONLY["c"] = ("Apache-2.0", hashlib.sha256(APACHE).hexdigest())
        self.assertError([self.crate("c", None, {"LICENSE": MIT}, "LICENSE")],
                          f"isn't the one read as Apache-2.0. Read it, then set its licence and sha256 "
                          f"({hashlib.sha256(MIT).hexdigest()})")

    def test_license_file_not_read(self):
        self.assertError([self.crate("c", None, {"TERMS": MIT}, "TERMS")],
                          "has no `license` in its Cargo.toml, only the file TERMS", "LICENSE_FILE_ONLY")


class RenderTest(Crates):
    def test_each_text_once(self):
        entries, errors, _ = self.collect(
            self.crate("b", "MIT OR Apache-2.0", {"LICENSE-MIT": MIT, "LICENSE-APACHE": APACHE}),
            self.crate("a-longer-name", "Apache-2.0", {"LICENSE.txt": APACHE.replace(b"\n", b"  \r\n")}))
        self.assertEqual(errors, [])
        text = rust_notices.render("ab" * 32, entries)
        self.assertIn(f"app/Cargo.lock, sha256 {'ab' * 32},", text)
        self.assertIn("\nb 1.0.0              MIT OR Apache-2.0\na-longer-name 1.0.0  Apache-2.0\n", text)
        self.assertEqual(text.count("Apache License"), 1)
        self.assertIn(f"{rust_notices.RULE}\nb 1.0.0: LICENSE-APACHE\na-longer-name 1.0.0: LICENSE.txt\n"
                      f"{rust_notices.THIN_RULE}\n\n{APACHE.decode().rstrip()}\n", text)
        self.assertEqual(text.count("Copyright (c) 2024 Someone"), 1)
        self.assertTrue(text.endswith("Permission is hereby granted...\n"))


class ShippedTest(Crates):
    """shipped(), with cargo's answers made up."""

    def cargo(self, tree, packages, members=("ours",)):
        metadata = {"workspace_members": list(members), "packages": packages}

        def cargo(root, command, *args):
            return tree if command == "tree" else json.dumps(metadata)
        return mock.patch.object(rust_notices, "cargo", cargo)

    def package(self, name, version, id=None, license="MIT", license_file=None):
        return {"id": id or f"registry+{name}@{version}", "name": name, "version": version, "license": license,
                "license_file": license_file, "manifest_path": f"/registry/{name}-{version}/Cargo.toml",
                "repository": None}

    def test_crates_in_the_build(self):
        tree = ("rega_bridge v0.0.1 (/repo/app/rust)\nserde_derive v1.0.228 (proc-macro)\n"
                "getrandom v0.3.4\ngetrandom v0.2.16\nserde_derive v1.0.228 (proc-macro) (*)\n\n")
        packages = [self.package("rega_bridge", "0.0.1", id="ours"), self.package("serde_derive", "1.0.228"),
                    self.package("getrandom", "0.3.4"), self.package("getrandom", "0.2.16"),
                    self.package("rcgen", "0.14.10")]
        with self.cargo(tree, packages):
            crates = rust_notices.shipped(self.root)
        self.assertEqual([str(c) for c in crates], ["getrandom 0.2.16", "getrandom 0.3.4", "serde_derive 1.0.228"])
        self.assertEqual(crates[0].path, Path("/registry/getrandom-0.2.16"))

    def test_crate_cargo_metadata_lacks(self):
        with self.cargo("tokio v1.0.0\n", [self.package("tokio", "1.0.1")]), self.assertRaises(SystemExit) as e:
            rust_notices.shipped(self.root)
        self.assertIn("('tokio', '1.0.0')", str(e.exception))

    def test_same_name_and_version_twice(self):
        packages = [self.package("tokio", "1.0.0"), self.package("tokio", "1.0.0", id="git+tokio@1.0.0")]
        with self.cargo("tokio v1.0.0\n", packages), self.assertRaises(SystemExit) as e:
            rust_notices.shipped(self.root)
        self.assertIn("two crates are tokio 1.0.0", str(e.exception))

    def test_unexpected_tree_output(self):
        with self.assertRaises(SystemExit):
            rust_notices.parse_tree("warning: something\n")


class RepoTest(unittest.TestCase):
    """The checked-in texts and tables agree."""

    def test_checked_in_texts(self):
        folders = {p.name for p in (rust_notices.ROOT / rust_notices.TEXTS).iterdir()}
        self.assertEqual(folders, set(rust_notices.UNSHIPPED))
        for name in folders:
            with self.subTest(name):
                self.assertTrue(any((rust_notices.ROOT / rust_notices.TEXTS / name).iterdir()))

    def test_tables_accepted(self):
        licenses = list(rust_notices.UNSHIPPED.values())
        licenses += [license for license, _ in rust_notices.LICENSE_FILE_ONLY.values()]
        for license in licenses:
            with self.subTest(license):
                self.assertTrue(rust_notices.accepted(license))


if __name__ == "__main__":
    unittest.main()
