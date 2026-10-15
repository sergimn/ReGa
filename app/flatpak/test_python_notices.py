"""Tests for python_notices.py, on a made-up site-packages and repo.

    python3 -m unittest discover -s app/flatpak
"""

import hashlib
import json
import shutil
import tempfile
import unittest
from email.message import Message
from pathlib import Path
from unittest import mock

import pins
import python_notices

MIT = b"MIT License\n\nCopyright (c) 2024 Someone\n\nPermission is hereby granted...\n"
GPL = b"                    GNU GENERAL PUBLIC LICENSE\n                       Version 3, 29 June 2007\n"
GPL_SHA256 = hashlib.sha256(GPL).hexdigest()
HASH = "0" * 64

LOCK = f"""# made up
ansible-core==2.21.4 \\
    --hash=sha256:{HASH}
    # via -r requirements.txt
pyyaml==6.0.3 \\
    --hash=sha256:{HASH}
    # via ansible-core
pywin32==311 ; sys_platform == 'win32' \\
    --hash=sha256:{HASH}
    # via something
"""


def metadata(**fields) -> Message:
    message = Message()
    for key, value in fields.items():
        for v in value if isinstance(value, list) else [value]:
            message[key.replace("_", "-")] = v
    return message


class LicenceOfTest(unittest.TestCase):
    def test_expression_first(self):
        self.assertEqual(python_notices.licence_of(metadata(
            License_Expression="MIT", License="Something else",
            Classifier="License :: OSI Approved :: BSD License")), "MIT")

    def test_short_licence_field(self):
        self.assertEqual(python_notices.licence_of(metadata(License="Apache-2.0")), "Apache-2.0")

    def test_classifiers_when_the_field_is_a_copyright_line(self):
        self.assertEqual(python_notices.licence_of(metadata(
            License="Copyright (c) 2013 Someone",
            Classifier=["Programming Language :: Python", "License :: OSI Approved :: MIT License"])),
            "MIT License")

    def test_classifiers_when_the_field_is_the_whole_text(self):
        self.assertEqual(python_notices.licence_of(metadata(
            License="MIT License\n\nPermission is hereby granted...",
            Classifier=["License :: OSI Approved :: MIT License", "License :: OSI Approved :: ISC License (ISCL)"])),
            "MIT License OR ISC License (ISCL)")

    def test_nothing(self):
        self.assertEqual(python_notices.licence_of(metadata(Name="x")), "unknown")


class WriteTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        # The made-up GPL text, read as the real one is.
        patch = mock.patch.dict(python_notices.LICENSE_FILE_ONLY,
                                {"ansible.windows": ("GPL-3.0-or-later", GPL_SHA256)}, clear=True)
        patch.start()
        self.addCleanup(patch.stop)
        base = Path(self.tmp.name)
        self.root = base / "repo"
        self.site = base / "site-packages"
        self.out = base / "out"
        self.put(self.root / pins.MANIFEST, "id: x\nruntime-version: '25.08'\n")
        self.put(self.root / pins.LOCK, LOCK)
        self.put(self.root / pins.COLLECTIONS,
                 "collections:\n  - name: ansible.windows\n    version: 3.8.0\n")
        self.dist("ansible_core", "ansible-core", "2.21.4", "License-Expression: GPL-3.0-or-later\n",
                  {"licenses/COPYING": GPL, "licenses/licenses/MIT-license.txt": MIT})
        self.dist("pyyaml", "PyYAML", "6.0.3", "License: MIT\n", {"licenses/LICENSE": MIT})
        self.collection("ansible", "windows", "3.8.0", {"COPYING": GPL})

    def put(self, path: Path, data) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        (path.write_bytes if isinstance(data, bytes) else path.write_text)(data)

    def dist(self, folder, name, version, fields, files) -> None:
        """Installs a package's metadata, instead of any version of it there."""
        for old in self.site.glob(f"{folder}-*.dist-info"):
            shutil.rmtree(old)
        dist_info = self.site / f"{folder}-{version}.dist-info"
        license_files = "".join(f"License-File: {f.removeprefix('licenses/')}\n" for f in files)
        self.put(dist_info / "METADATA",
                 f"Metadata-Version: 2.4\nName: {name}\nVersion: {version}\n{fields}{license_files}\nReadme\n")
        for path, data in files.items():
            self.put(dist_info / path, data)

    def collection(self, namespace, name, version, files, license=(), license_file="COPYING") -> None:
        folder = self.site / "ansible_collections" / namespace / name
        info = {"namespace": namespace, "name": name, "version": version,
                "license": list(license), "license_file": license_file}
        self.put(folder / "MANIFEST.json", json.dumps({"collection_info": info}))
        self.put(folder / "README.md", "# Readme\n")
        for path, data in files.items():
            self.put(folder / path, data)

    def write(self) -> list[str]:
        return python_notices.write(self.root, self.site, self.out)

    def written(self) -> str:
        self.assertEqual(self.write(), [])
        return (self.out / python_notices.PACKAGES_FILE).read_text(encoding="utf-8")

    def test_lists_packages_and_collections_with_their_texts_once(self):
        text = self.written()
        lock_sha256 = hashlib.sha256((self.root / pins.LOCK).read_bytes()).hexdigest()
        self.assertIn(f"{pins.LOCK}, sha256 {lock_sha256},", text)
        self.assertIn("\nPython packages\n" + "-" * 78 + "\n\nansible-core 2.21.4  GPL-3.0-or-later\n"
                      "PyYAML 6.0.3         MIT\n", text)
        self.assertIn("\nAnsible collections\n" + "-" * 78 + "\n\nansible.windows 3.8.0  GPL-3.0-or-later\n", text)
        # The GPL ansible-core and the collection ship, once, under both.
        self.assertEqual(text.count("GNU GENERAL PUBLIC LICENSE"), 1)
        self.assertIn("=" * 78 + "\nansible-core 2.21.4: licenses/COPYING\nansible.windows 3.8.0: COPYING\n"
                      + "-" * 78 + "\n\n                    GNU GENERAL PUBLIC LICENSE\n", text)
        self.assertEqual(text.count("Permission is hereby granted"), 1)
        self.assertIn("ansible-core 2.21.4: licenses/licenses/MIT-license.txt\n"
                      "PyYAML 6.0.3: licenses/LICENSE\n", text)
        self.assertTrue(text.endswith("Permission is hereby granted...\n"))

    def test_licence_file_at_the_top_of_dist_info(self):
        # Older wheels put License-File files next to METADATA.
        self.dist("pyyaml", "PyYAML", "6.0.3", "License: MIT\n", {"LICENSE": MIT})
        self.assertIn("PyYAML 6.0.3: LICENSE\n", self.written())

    def test_licence_files_without_license_file_lines(self):
        shutil.rmtree(self.site / "pyyaml-6.0.3.dist-info")
        dist_info = self.site / "pyyaml-6.0.3.dist-info"
        self.put(dist_info / "METADATA", "Metadata-Version: 2.1\nName: PyYAML\nVersion: 6.0.3\nLicense: MIT\n\n")
        self.put(dist_info / "LICENSE.txt", MIT)
        self.put(dist_info / "RECORD", "")
        self.assertIn("PyYAML 6.0.3: LICENSE.txt\n", self.written())

    def test_a_package_without_a_text_fails(self):
        self.dist("pyyaml", "PyYAML", "6.0.3", "License: MIT\n", {})
        errors = self.write()
        self.assertTrue(any("PyYAML 6.0.3 ships no licence text" in e for e in errors), errors)
        self.assertFalse(self.out.exists())

    def test_installed_has_to_match_the_lock(self):
        self.dist("pyyaml", "PyYAML", "6.0.2", "License: MIT\n", {"licenses/LICENSE": MIT})
        self.dist("idna", "idna", "3.19", "License-Expression: BSD-3-Clause\n", {"licenses/LICENSE.md": MIT})
        errors = self.write()
        self.assertIn(f"{pins.LOCK} has pyyaml==6.0.3, but PyYAML 6.0.2 is installed.", errors)
        self.assertIn(f"idna 3.19 is installed, but {pins.LOCK} doesn't have it.", errors)
        self.assertTrue(errors[-1].endswith("run `python3 app/flatpak/pins.py update`."))
        self.assertFalse(self.out.exists())

    def test_a_locked_package_missing(self):
        shutil.rmtree(self.site / "pyyaml-6.0.3.dist-info")
        # pywin32 is only for Windows, so not wanted here.
        self.assertIn(f"{pins.LOCK} has pyyaml==6.0.3, which isn't installed.", self.write())

    def test_collections_have_to_match_requirements_yml(self):
        self.collection("ansible", "windows", "3.7.0", {"COPYING": GPL})
        self.collection("community", "general", "1.0.0", {"COPYING": GPL}, license=["GPL-3.0-or-later"])
        errors = self.write()
        self.assertIn(f"{pins.COLLECTIONS} has ansible.windows 3.8.0, but ansible.windows 3.7.0 is installed.", errors)
        self.assertIn(f"community.general 1.0.0 is installed, but {pins.COLLECTIONS} doesn't have it.", errors)

    def test_a_collection_without_a_licence_name(self):
        self.put(self.root / pins.COLLECTIONS, "collections:\n  - name: ansible.windows\n    version: 3.8.0\n"
                 "  - name: community.general\n    version: 1.0.0\n")
        self.collection("community", "general", "1.0.0", {"COPYING": GPL})
        errors = self.write()
        self.assertTrue(any(e.startswith("community.general 1.0.0 names no licence") and GPL_SHA256 in e
                            for e in errors), errors)

    def test_a_collection_with_a_licence_name(self):
        self.put(self.root / pins.COLLECTIONS, "collections:\n  - name: ansible.windows\n    version: 3.8.0\n"
                 "  - name: community.general\n    version: 1.0.0\n")
        self.collection("community", "general", "1.0.0", {"LICENSE": MIT}, license=["MIT", "Apache-2.0"],
                        license_file=None)
        self.assertIn("community.general 1.0.0  MIT OR Apache-2.0\n", self.written())

    def test_a_changed_licence_file_fails(self):
        self.collection("ansible", "windows", "3.8.0", {"COPYING": MIT})
        errors = self.write()
        self.assertTrue(any(e.startswith("ansible.windows 3.8.0: its licence file COPYING isn't the one read")
                            for e in errors), errors)

    def test_nothing_installed(self):
        self.assertEqual(python_notices.write(self.root, self.site / "missing", self.out),
                         [f"Nothing is installed in {self.site / 'missing'}."])


class RepoTest(unittest.TestCase):
    def test_licence_file_only_covers_the_pinned_collections(self):
        # So a collection dropped from requirements.yml doesn't leave a stale entry.
        self.assertLessEqual(python_notices.LICENSE_FILE_ONLY.keys(), pins.collection_pins(pins.ROOT).keys())


if __name__ == "__main__":
    unittest.main()
