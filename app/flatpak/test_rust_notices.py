"""Tests for rust_notices.py, on made-up crates; no cargo needed.

    python3 -m unittest discover -s app/flatpak
"""

import hashlib
import io
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
BSD = b"Copyright (c) 2017 Someone\n\nRedistribution and use in source and binary forms...\n"
COMMIT = "0123456789abcdef0123456789abcdef01234567"


class AcceptedTest(unittest.TestCase):
    def test_permissive(self):
        for expression in ("MIT", "MIT OR Apache-2.0", "Apache-2.0 / MIT", "MIT/Apache-2.0",
                           "(MIT OR Apache-2.0) AND Unicode-3.0", "Apache-2.0 WITH LLVM-exception",
                           "ISC AND (Apache-2.0 OR ISC) AND (Apache-2.0 OR ISC OR MIT-0)", "mit", "MIT or apache-2.0"):
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
        self.assertFalse(rust_notices.accepted("mit and gpl-3.0-only"))

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
        for table in ("UNSHIPPED", "LEFT_OUT", "LICENSE_FILE_ONLY"):
            patcher = mock.patch.dict(getattr(rust_notices, table), clear=True)
            patcher.start()
            self.addCleanup(patcher.stop)

    def crate(self, name, license="MIT", files=None, license_file=None, version="1.0.0", commit=COMMIT):
        path = self.tmp / "registry" / f"{name}-{version}"
        path.mkdir(parents=True)
        files = dict(files or {})
        if commit:
            vcs = {"git": {"sha1": commit}, "path_in_vcs": f"crates/{name}"}
            files.setdefault(".cargo_vcs_info.json", json.dumps(vcs).encode())
        for file, data in files.items():
            (path / file).parent.mkdir(parents=True, exist_ok=True)
            (path / file).write_bytes(data)
        return Crate(name, version, license, license_file, path, f"https://example.com/{name}")

    def check_in(self, name, license, files, commit=COMMIT):
        rust_notices.UNSHIPPED[name] = (license, commit)
        self.add_texts(name, files)

    def add_texts(self, name, files):
        for file, data in files.items():
            path = self.root / rust_notices.TEXTS / name / file
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)

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

    def test_own_licence(self):
        crate = self.crate("c", license_file="legal/terms.txt", files={
            "LICENSE": b"x", "LICENSES/MIT.txt": b"x", "legal/terms.txt": b"x", "vendor/LICENSE": b"x",
            "vendor/LICENSES/MIT.txt": b"x"})
        own = {p.relative_to(crate.path).as_posix(): rust_notices.own_license(crate, p)
               for p in rust_notices.license_files(crate.path, crate.license_file)}
        self.assertEqual(own, {"LICENSE": True, "LICENSES/MIT.txt": True, "legal/terms.txt": True,
                               "vendor/LICENSE": False, "vendor/LICENSES/MIT.txt": False})

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
                         f"Copy it from https://example.com/c at commit {COMMIT} (the crate is in crates/c/) "
                         "into app/flatpak/rust-licenses/c/", "UNSHIPPED")

    def test_no_text_nor_commit(self):
        self.assertError([self.crate("c", "MIT", {"README.md": b"x"}, commit=None)],
                         "Copy it from https://example.com/c, at the commit the release was made from, into "
                         "app/flatpak/rust-licenses/c/")

    def test_vendored_text_is_not_its_own(self):
        self.assertError([self.crate("c", "MIT", {"vendor/LICENSE": b"BSD"})], "ships no licence text of its own")

    def test_text_checked_in(self):
        self.check_in("c", "MIT", {"LICENSE": MIT})
        [entry], errors, notes = self.collect(self.crate("c", "MIT", {"vendor/LICENSE": b"BSD"}))
        self.assertEqual((errors, notes), ([], []))
        self.assertEqual([t.source for t in entry.texts],
                         ["app/flatpak/rust-licenses/c/LICENSE, from its repository at 0123456", "vendor/LICENSE"])
        self.assertEqual(entry.texts[0].body, MIT.decode())

    def test_checked_in_text_from_an_older_release(self):
        self.check_in("c", "MIT", {"LICENSE": MIT})
        [entry], errors, notes = self.collect(self.crate("c", "MIT", commit="f" * 40))
        self.assertEqual(errors, [])
        self.assertEqual(len(entry.texts), 1)
        [note] = notes
        self.assertIn(f"published from commit {'f' * 40}, and its text in app/flatpak/rust-licenses/c/ taken at "
                      f"{COMMIT}", note)

    def test_checked_in_text_was_for_another_licence(self):
        self.check_in("c", "MIT", {"LICENSE": MIT})
        self.assertError([self.crate("c", "Apache-2.0")], "says `Apache-2.0` now", "taken for `MIT`",
                         f"from commit {COMMIT}")

    def test_checked_in_folder_empty(self):
        rust_notices.UNSHIPPED["c"] = ("MIT", COMMIT)
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
        crate = self.crate("c", None, {"TERMS": MIT}, "TERMS")
        self.assertError([crate], f"has no `license` in its Cargo.toml, only the file {crate.path / 'TERMS'}",
                         f"LICENSE_FILE_ONLY in app/flatpak/rust_notices.py, with its licence and the file's "
                         f"sha256 ({hashlib.sha256(MIT).hexdigest()})")

    def test_vendored_licence_left_out(self):
        rust_notices.LEFT_OUT["c"] = ("1.0.0", ("third_party/jitter",))
        self.add_texts("c", {"third_party/jitter/LICENSE": BSD})
        [entry], errors, notes = self.collect(
            self.crate("c", "MIT AND BSD-3-Clause", {"LICENSE": MIT, "third_party/jitter/jitter.c": b""}))
        self.assertEqual((errors, notes), ([], []))
        self.assertEqual([t.source for t in entry.texts], [
            "LICENSE", "app/flatpak/rust-licenses/c/third_party/jitter/LICENSE, which the crate leaves out"])
        self.assertEqual(entry.texts[1].body, BSD.decode())

    def test_vendored_licence_left_out_changes(self):
        rust_notices.LEFT_OUT["c"] = ("0.9.0", ("third_party/jitter", "third_party/gone", "third_party/empty"))
        self.add_texts("c", {"third_party/jitter/LICENSE": BSD, "third_party/gone/LICENSE": BSD})
        _, errors, notes = self.collect(self.crate("c", "MIT", {
            "LICENSE": MIT, "third_party/jitter/LICENSE": BSD, "third_party/empty/x.c": b""}))
        self.assertEqual(errors, ["c 1.0.0: nothing in app/flatpak/rust-licenses/c/third_party/empty/"])
        self.assertEqual(len(notes), 3, notes)
        self.assertIn("were taken for 0.9.0", notes[0])
        self.assertIn("ships a licence in third_party/jitter/ now", notes[1])
        self.assertIn("has no third_party/gone/ any more", notes[2])


class RenderTest(Crates):
    def test_each_text_once(self):
        entries, errors, _ = self.collect(
            self.crate("b", "MIT OR Apache-2.0", {"LICENSE-MIT": MIT, "LICENSE-APACHE": APACHE}),
            self.crate("a-longer-name", "Apache-2.0", {"LICENSE.txt": APACHE.replace(b"\n", b"  \r\n")}))
        self.assertEqual(errors, [])
        text = rust_notices.render("ab" * 32, "rustc 1.0.0 (abc 2026-01-01)", entries)
        self.assertIn(f"app/Cargo.lock, sha256 {'ab' * 32},", text)
        self.assertIn("its notices are in rust-std.html,\nfrom rustc 1.0.0 (abc 2026-01-01).", text)
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


class WriteTest(Crates):
    """main(), with the crates and the toolchain made up."""

    def test_write(self):
        (self.root / rust_notices.LOCK).parent.mkdir(parents=True, exist_ok=True)
        (self.root / rust_notices.LOCK).write_bytes(b"lock")
        std = self.tmp / "sysroot" / rust_notices.STD_NOTICES
        std.parent.mkdir(parents=True)
        std.write_bytes(b"<html>std</html>")
        out = self.tmp / "out" / "licenses"
        crates = [self.crate("c", "MIT", {"LICENSE": MIT})]
        with (mock.patch.object(rust_notices, "ROOT", self.root),
              mock.patch.object(rust_notices, "shipped", return_value=crates),
              mock.patch.object(rust_notices, "rust_std", return_value=("rustc 1.0.0", std)),
              mock.patch("sys.stdout")):
            self.assertEqual(rust_notices.main(["write", str(out)]), 0)
            self.assertEqual(rust_notices.main(["check"]), 0)
            std.unlink()
            with mock.patch("sys.stderr"):
                self.assertEqual(rust_notices.main(["check"]), 1)
        self.assertEqual(sorted(p.name for p in out.iterdir()), ["rust-crates.txt", "rust-std.html"])
        self.assertEqual((out / "rust-std.html").read_bytes(), b"<html>std</html>")
        text = (out / "rust-crates.txt").read_text()
        self.assertIn(f"sha256 {hashlib.sha256(b'lock').hexdigest()},", text)
        self.assertIn("c 1.0.0: LICENSE\n", text)


class NotesTest(Crates):
    """main() shows notes as warnings on GitHub, and passes."""

    def run_main(self, environ):
        (self.root / rust_notices.LOCK).parent.mkdir(parents=True, exist_ok=True)
        (self.root / rust_notices.LOCK).write_bytes(b"lock")
        std = self.tmp / rust_notices.STD_NOTICES
        std.parent.mkdir(parents=True)
        std.write_bytes(b"std")
        self.check_in("c", "MIT", {"LICENSE": MIT}, commit="f" * 40)
        stderr = io.StringIO()
        with (mock.patch.object(rust_notices, "ROOT", self.root),
              mock.patch.object(rust_notices, "shipped", return_value=[self.crate("c")]),
              mock.patch.object(rust_notices, "rust_std", return_value=("rustc 1.0.0", std)),
              mock.patch.dict("os.environ", environ, clear=True),
              mock.patch("sys.stdout"), mock.patch("sys.stderr", stderr)):
            self.assertEqual(rust_notices.main(["check"]), 0)
        return stderr.getvalue()

    def test_on_github(self):
        self.assertRegex(self.run_main({"GITHUB_ACTIONS": "true"}), r"^::warning::c 1\.0\.0 was published from commit")

    def test_elsewhere(self):
        self.assertRegex(self.run_main({}), r"^note: c 1\.0\.0 was published from commit")


class RustStdTest(unittest.TestCase):
    def test_cargokit_toolchain(self):
        calls = []

        def run(root, *command):
            calls.append(command)
            return "/sysroot\n" if "--print" in command else "rustc 1.0.0\n"
        with mock.patch.object(rust_notices, "run", run), mock.patch("shutil.which", return_value="/bin/rustup"):
            self.assertEqual(rust_notices.rust_std(Path("/repo")),
                             ("rustc 1.0.0", Path("/sysroot") / rust_notices.STD_NOTICES))
        self.assertEqual(calls, [("rustup", "run", "stable", "rustc", "--version"),
                                 ("rustup", "run", "stable", "rustc", "--print", "sysroot")])
        calls.clear()
        with mock.patch.object(rust_notices, "run", run), mock.patch("shutil.which", return_value=None):
            rust_notices.rust_std(Path("/repo"))
        self.assertEqual(calls[0], ("rustc", "--version"))


class RepoTest(unittest.TestCase):
    """The checked-in texts and tables agree."""

    def test_checked_in_texts(self):
        texts = rust_notices.ROOT / rust_notices.TEXTS
        self.assertEqual({p.name for p in texts.iterdir()}, set(rust_notices.UNSHIPPED) | set(rust_notices.LEFT_OUT))
        for name in rust_notices.UNSHIPPED:
            with self.subTest(name):
                self.assertTrue(any(p.is_file() for p in (texts / name).iterdir()))
        for name, (_, folders) in rust_notices.LEFT_OUT.items():
            for folder in folders:
                with self.subTest(name, folder=folder):
                    self.assertTrue(any(p.is_file() for p in (texts / name / folder).iterdir()))

    def test_tables_accepted(self):
        licenses = [license for license, _ in rust_notices.UNSHIPPED.values()]
        licenses += [license for license, _ in rust_notices.LICENSE_FILE_ONLY.values()]
        for license in licenses:
            with self.subTest(license):
                self.assertTrue(rust_notices.accepted(license))


if __name__ == "__main__":
    unittest.main()
