"""Tests for pins.py, on a copy of the repo's real pin files.

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
        for path in (pins.MANIFEST, pins.ANSIBLE_MODULE, pins.REQUIREMENTS, pins.LOCK,
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

    def locked(self, name):
        [entry] = pins.locked_packages(self.root)[name]
        return entry.version

    def test_repo_matches(self):
        self.assertEqual(pins.check(pins.ROOT), [])

    def test_lock_bumped(self):
        self.edit(pins.LOCK, r"^pywinrm==\S+", "pywinrm==9.9.9")
        self.assertDrift("requirements.lock has pywinrm==9.9.9; app/flatpak/ansible.yml has 0.5",
                         "pins.py update")

    def test_lock_package_added(self):
        with open(self.root / pins.LOCK, "a") as f:
            f.write("boto3==1.0.0 \\\n    --hash=sha256:" + "0" * 64 + "\n")
        self.assertDrift("requirements.lock has boto3==1.0.0; app/flatpak/ansible.yml has nothing")

    def test_lock_package_removed(self):
        self.edit(pins.LOCK, r"^xmltodict==.*\n(?:\s+.*\n)*", "")
        self.assertDrift("ansible.yml has xmltodict-", "which infra/ansible/requirements.lock doesn't have")

    def test_lock_forks_by_python(self):
        # uv can lock a package twice, for different Pythons: the runtime's wins.
        xmltodict = self.locked("xmltodict")
        self.edit(pins.LOCK, r"^(xmltodict==\S+) ", r"\1 ; python_full_version >= '3.13' ")
        with open(self.root / pins.LOCK, "a") as f:
            f.write("xmltodict==0.1 ; python_full_version < '3.13' \\\n    --hash=sha256:" + "0" * 64 + "\n")
        self.assertEqual(pins.check(self.root), [])
        self.assertEqual(pins.locked_for(self.root, pins.RUNTIMES["25.08"])["xmltodict"].version, xmltodict)
        # Two entries that both apply can't be told apart.
        self.edit(pins.LOCK, r"^xmltodict==0\.1 ; python_full_version < '3\.13'", "xmltodict==0.1 ; os_name == 'posix'")
        with self.assertRaises(SystemExit):
            pins.check(self.root)

    def test_wheel_not_in_lock(self):
        self.edit(pins.ANSIBLE_MODULE, r"(pywinrm-[^\"]+\"\n\s+sha256:) \S+", r"\1 " + "0" * 64)
        self.assertDrift("pywinrm-", "whose sha256 isn't one of")

    def test_marker_for_another_platform(self):
        # sspilib is Windows-only in the lock, so the Flatpak skips it...
        self.assertNotIn("sspilib", (self.root / pins.ANSIBLE_MODULE).read_text())
        # ...until the lock says otherwise.
        self.edit(pins.LOCK, r"^(sspilib==\S+) ;[^\\]*", r"\1 ")
        self.assertDrift(f"has sspilib=={self.locked('sspilib')}; app/flatpak/ansible.yml has nothing")

    def test_unknown_marker(self):
        self.edit(pins.LOCK, r"^(sspilib==\S+ ;)[^\\]*", r"\1 platform_release == '6' ")
        with self.assertRaises(SystemExit):
            pins.check(self.root)

    def test_markers(self):
        env = pins.marker_environment(pins.Runtime("3.13", (2, 42)))
        for marker, applies in [
            ("", True),
            ("sys_platform == 'win32'", False),
            ("platform_python_implementation != 'PyPy'", True),
            ("implementation_name != 'PyPy' and sys_platform == 'win32'", False),
            ("sys_platform == 'win32' or python_version >= '3.10'", True),
            ("(sys_platform == 'win32' or python_version < '3.13') and os_name == 'posix'", False),
            ('python_version <= "3.13"', True),
            ("python_version == '3.13.0'", True),
            ("python_version == '3.13.0.0'", True),
            ("python_version != '3.13'", False),
            ("python_version != '3.12.*'", True),
            # The runtime's patch release isn't fixed, but these hold for every one.
            ("python_full_version >= '3.13'", True),
            ("python_full_version < '3.13'", False),
            ("python_full_version == '3.13.*'", True),
            ("python_full_version < '3.10'", False),
        ]:
            with self.subTest(marker):
                self.assertEqual(pins.marker_applies(marker, env), applies)
        for marker in ("sys_platform == 'linux' and", "(os_name == 'posix'", "sys_platform ~= 'linux'",
                       "sys_platform < 'linux'", "python_version < '3.13.*'",
                       "python_full_version >= '3.13.2'", "python_full_version == '3.13.5'",
                       "python_full_version != '3.13.5.*'"):
            with self.subTest(marker), self.assertRaises(SystemExit):
                pins.marker_applies(marker, env)

    def test_requirements_ahead_of_lock(self):
        self.edit(pins.REQUIREMENTS, r"^pywinrm==\S+", "pywinrm==9.9.9")
        self.assertDrift(f"pins pywinrm==9.9.9, but infra/ansible/requirements.lock has "
                         f"pywinrm=={self.locked('pywinrm')}; regenerate the lock")

    def test_pin_removed_from_requirements(self):
        self.edit(pins.REQUIREMENTS, r"^pywinrm==\S+\n", "")
        self.assertDrift("requirements.lock has pywinrm from infra/ansible/requirements.txt, "
                         "which no longer pins it")

    def test_pin_not_marked_as_direct(self):
        self.edit(pins.LOCK, r"(^pywinrm==[^#]*)# via -r requirements\.txt", r"\1# via ansible-core")
        self.assertDrift("doesn't say pywinrm comes from infra/ansible/requirements.txt")

    def test_lock_regenerated_elsewhere(self):
        lock = self.root / pins.LOCK
        lock.write_text(lock.read_text().replace("-r requirements.txt", "-r infra/ansible/requirements.txt"))
        self.assertEqual(pins.check(self.root), [])

    def test_same_release_written_longer(self):
        self.edit(pins.REQUIREMENTS, r"^pywinrm==(\S+)", r"pywinrm==\1.0")
        self.edit(pins.LOCK, r"^pywinrm==(\S+)", r"pywinrm==\1.0")
        self.assertEqual(pins.check(self.root), [])

    def test_loose_python_pin(self):
        for loose in ("pywinrm>=0.5", "pywinrm==0.*", "pywinrm===0.5.0"):
            with self.subTest(loose):
                self.edit(pins.REQUIREMENTS, r"^pywinrm\S+", loose)
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
        self.assertDrift("-cp312-cp312-", "doesn't install on the 25.08 runtime")

    def test_unknown_runtime(self):
        self.edit(pins.MANIFEST, r"^runtime-version: .*", "runtime-version: '99.08'")
        self.assertDrift("Runtime 99.08 isn't in RUNTIMES")

    def test_module_not_included(self):
        self.edit(pins.MANIFEST, r"^\s*- ansible\.yml\n", "")
        self.assertDrift("doesn't include ansible.yml")

    def test_licence_notices_from_another_python(self):
        self.edit(pins.MANIFEST, r"/app/lib/python3\.13/site-packages", "/app/lib/python3.12/site-packages")
        self.assertDrift("doesn't run python_notices.py on the runtime's /app/lib/python3.13/site-packages")


class BestWheelTest(unittest.TestCase):
    runtime = pins.Runtime("3.13", (2, 42))

    def best(self, *filenames):
        return pins.best_wheel(filenames, self.runtime)

    def test_newest_glibc_the_runtime_has(self):
        self.assertEqual(self.best(
            "x-1-cp313-cp313-manylinux_2_17_x86_64.manylinux2014_x86_64.whl",
            "x-1-cp313-cp313-manylinux_2_34_x86_64.whl",
            "x-1-cp313-cp313-manylinux_2_43_x86_64.whl",
            "x-1-cp313-cp313-musllinux_1_2_x86_64.whl",
            "x-1-cp313-cp313-manylinux_2_39_aarch64.whl",
            "x-1-cp313-cp313-linux_x86_64.whl",
        ), "x-1-cp313-cp313-manylinux_2_34_x86_64.whl")

    def test_own_abi_then_abi3_then_pure(self):
        own = "x-1-cp313-cp313-manylinux_2_17_x86_64.whl"
        abi3 = "x-1-cp311-abi3-manylinux_2_34_x86_64.whl"
        pure = "x-1-py3-none-any.whl"
        self.assertEqual(self.best(pure, abi3, own), own)
        self.assertEqual(self.best(pure, abi3), abi3)
        self.assertEqual(self.best(pure), pure)
        self.assertEqual(self.best("x-1-py2.py3-none-any.whl"), "x-1-py2.py3-none-any.whl")

    def test_nothing_that_loads(self):
        self.assertIsNone(self.best(
            "x-1-cp314-cp314-manylinux_2_17_x86_64.whl",
            "x-1-cp314-abi3-manylinux_2_17_x86_64.whl",
            "x-1-cp312-cp312-manylinux_2_17_x86_64.whl",
            "x-1-cp313-cp313-win_amd64.whl",
            "x-1-py2-none-any.whl",
            "x-1-cp313-cp313-any.whl",
            "x-1-cp313-abi3-any.whl",
        ))


if __name__ == "__main__":
    unittest.main()
