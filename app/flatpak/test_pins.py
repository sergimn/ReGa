"""Tests for pins.py check, on a copy of the repo's real pin files.

    python3 -m unittest discover -s app/flatpak
"""

import re
import shutil
import tempfile
import unittest
from pathlib import Path

import pins


class CheckTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        for path in (pins.MANIFEST, pins.ANSIBLE_MODULE, pins.REQUIREMENTS,
                     pins.COLLECTIONS, pins.TERRAFORM_WORKFLOW):
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(pins.ROOT / path, self.root / path)

    def edit(self, path, pattern, replacement):
        file = self.root / path
        text, count = re.subn(pattern, replacement, file.read_text(), count=1, flags=re.M)
        self.assertEqual(count, 1, f"{pattern!r} not found in {path}")
        file.write_text(text)

    def assertDrift(self, *fragments):
        errors = "\n".join(pins.check(self.root))
        for fragment in fragments:
            self.assertIn(fragment, errors)

    def test_repo_matches(self):
        self.assertEqual(pins.check(pins.ROOT), [])

    def test_python_pin_bumped(self):
        self.edit(pins.REQUIREMENTS, r"^pywinrm==\S+", "pywinrm==9.9.9")
        self.assertDrift("pins pywinrm==9.9.9", "pins.py update")

    def test_python_pin_added(self):
        with open(self.root / pins.REQUIREMENTS, "a") as f:
            f.write("boto3==1.0.0\n")
        self.assertDrift("pins boto3==1.0.0; app/flatpak/ansible.yml has nothing")

    def test_loose_python_pin(self):
        self.edit(pins.REQUIREMENTS, r"^pywinrm==\S+", "pywinrm>=0.5")
        with self.assertRaises(SystemExit):
            pins.check(self.root)

    def test_collection_bumped(self):
        self.edit(pins.COLLECTIONS, r"(name: ansible\.windows\s+version:) \S+", r"\1 9.9.9")
        self.assertDrift("wants ansible-windows-9.9.9.tar.gz", "has ansible-windows-")

    def test_collection_without_version(self):
        self.edit(pins.COLLECTIONS, r"^(\s+)version: \S+\n", "")
        with self.assertRaises(SystemExit):
            pins.check(self.root)

    def test_terraform_out_of_step(self):
        self.edit(pins.TERRAFORM_WORKFLOW, r"(terraform_version:) \S+", r"\1 1.0.0")
        self.assertDrift("CI tests the module with 1.0.0")

    def test_wheel_for_another_python(self):
        self.edit(pins.ANSIBLE_MODULE, r"(pyyaml-[^-]+)-cp313-cp313-", r"\1-cp312-cp312-")
        self.assertDrift("cp312 wheel of pyyaml")

    def test_unknown_runtime(self):
        self.edit(pins.MANIFEST, r"^runtime-version: .*", "runtime-version: '99.08'")
        self.assertDrift("Runtime 99.08 isn't in RUNTIME_PYTHON")

    def test_module_not_included(self):
        self.edit(pins.MANIFEST, r"^\s*- ansible\.yml\n", "")
        self.assertDrift("doesn't include ansible.yml")


if __name__ == "__main__":
    unittest.main()
