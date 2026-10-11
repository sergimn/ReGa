#!/usr/bin/env python3
"""Pins for the tools the Flatpak bundles next to the app.

    python3 app/flatpak/pins.py update   # regenerate ansible.yml (needs network and pip)
    python3 app/flatpak/pins.py check    # offline; CI runs it

infra/ansible/requirements.txt and requirements.yml are the source of truth
for Ansible, and .github/workflows/terraform.yml for Terraform. `update` turns
the Ansible pins into the manifest module ansible.yml: every Python wheel they
need (resolved for the runtime's Python) and the Galaxy collections, each with
its URL and sha256. `check` fails when the manifest no longer matches them.
Only stdlib, so it runs on a bare CI runner.
"""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
import sys
import tempfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "app/flatpak/io.github.sergimn.rega.yml"
ANSIBLE_MODULE = "app/flatpak/ansible.yml"
REQUIREMENTS = "infra/ansible/requirements.txt"
COLLECTIONS = "infra/ansible/requirements.yml"
TERRAFORM_WORKFLOW = ".github/workflows/terraform.yml"

# The Python each runtime ships. Compiled wheels only load on that version, so
# a runtime bump needs a new entry here and `update`.
RUNTIME_PYTHON = {"25.08": "3.13"}

# The manifest only builds for x86_64 (Terraform and the Flutter bundle too).
WHEEL_PLATFORMS = [
    "manylinux_2_28_x86_64",
    "manylinux_2_17_x86_64",
    "manylinux2014_x86_64",
]

GALAXY = "https://galaxy.ansible.com/api/v3/plugin/ansible/content/published/collections/index"


@dataclass(frozen=True)
class Source:
    filename: str
    url: str
    sha256: str


def normalize(name: str) -> str:
    """PEP 503 name, so ansible-core, ansible_core and Ansible.Core match."""
    return re.sub(r"[-_.]+", "-", name).lower()


def read(root: Path, path: str) -> str:
    return (root / path).read_text(encoding="utf-8")


def runtime_version(root: Path) -> str:
    m = re.search(r"^runtime-version:\s*['\"]?([\d.]+)", read(root, MANIFEST), re.M)
    if not m:
        raise SystemExit(f"{MANIFEST}: no runtime-version")
    return m.group(1)


def python_pins(root: Path) -> dict[str, str]:
    """name -> version for each `name==version` line of requirements.txt."""
    pins = {}
    for line in read(root, REQUIREMENTS).splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        m = re.fullmatch(r"([A-Za-z0-9][A-Za-z0-9._-]*)==([^\s;]+)", line)
        if not m:
            raise SystemExit(f"{REQUIREMENTS}: pin every package exactly, not {line!r}")
        pins[normalize(m.group(1))] = m.group(2)
    if not pins:
        raise SystemExit(f"{REQUIREMENTS}: no packages")
    return pins


def collection_pins(root: Path) -> dict[str, str]:
    """namespace.name -> version from requirements.yml.

    Reads only the `- name: x` / `version: y` shape that file uses, and
    refuses anything else rather than guess.
    """
    text = read(root, COLLECTIONS)
    pairs = re.findall(r"^\s*-\s*name:\s*(\S+)\s*\n\s+version:\s*['\"]?([^\s'\"]+)", text, re.M)
    names = re.findall(r"^\s*-\s*name:", text, re.M)
    if not pairs or len(pairs) != len(names):
        raise SystemExit(f"{COLLECTIONS}: give every collection a name and then a version")
    return dict(pairs)


def terraform_versions(root: Path) -> tuple[str, str]:
    """(bundled, CI) Terraform versions."""
    bundled = re.search(r"releases\.hashicorp\.com/terraform/([\d.]+)/", read(root, MANIFEST))
    ci = re.search(r"terraform_version:\s*['\"]?([\d.]+)", read(root, TERRAFORM_WORKFLOW))
    if not bundled or not ci:
        raise SystemExit(f"Terraform version not found in {MANIFEST} or {TERRAFORM_WORKFLOW}")
    return bundled.group(1), ci.group(1)


def wheel_parts(filename: str) -> tuple[str, str, str]:
    """(normalized name, version, ABI tag) of a wheel file name."""
    parts = filename[: -len(".whl")].split("-")
    # name-version[-build]-python-abi-platform
    return normalize(parts[0]), parts[1], parts[-2]


def module_sources(text: str) -> list[str]:
    """File names of the sources in a generated ansible.yml."""
    return [url.rsplit("/", 1)[1] for url in re.findall(r"^\s*url:\s*\"?([^\s\"]+)", text, re.M)]


def check(root: Path) -> list[str]:
    """Every way the manifest has drifted from the repo's pins."""
    errors = []

    bundled, ci = terraform_versions(root)
    if bundled != ci:
        errors.append(f"The Flatpak bundles Terraform {bundled} but CI tests the module with {ci}.")

    runtime = runtime_version(root)
    python = RUNTIME_PYTHON.get(runtime)
    if python is None:
        errors.append(f"Runtime {runtime} isn't in RUNTIME_PYTHON in pins.py; add its Python.")

    module = root / ANSIBLE_MODULE
    if not module.exists():
        return errors + [f"{ANSIBLE_MODULE} is missing; run pins.py update."]
    text = module.read_text(encoding="utf-8")
    if not re.search(rf"^\s*-\s*\"?{re.escape(Path(ANSIBLE_MODULE).name)}\"?\s*$", read(root, MANIFEST), re.M):
        errors.append(f"{MANIFEST} doesn't include {Path(ANSIBLE_MODULE).name} in its modules.")
    files = module_sources(text)

    wheels = {}
    for name, version, abi in map(wheel_parts, (f for f in files if f.endswith(".whl"))):
        wheels[name] = version
        # A cp313 ABI only loads on 3.13; abi3 and none load on any 3.x.
        if python and abi.startswith("cp") and abi != "cp" + python.replace(".", ""):
            errors.append(f"{ANSIBLE_MODULE} has a {abi} wheel of {name}; the runtime has Python {python}.")
    for name, version in python_pins(root).items():
        if wheels.get(name) != version:
            have = wheels.get(name, "nothing")
            errors.append(f"{REQUIREMENTS} pins {name}=={version}; {ANSIBLE_MODULE} has {have}.")

    tarballs = {f for f in files if f.endswith(".tar.gz")}
    wanted = {f"{n.replace('.', '-')}-{v}.tar.gz" for n, v in collection_pins(root).items()}
    for missing in sorted(wanted - tarballs):
        errors.append(f"{COLLECTIONS} wants {missing}, which {ANSIBLE_MODULE} doesn't have.")
    for extra in sorted(tarballs - wanted):
        errors.append(f"{ANSIBLE_MODULE} has {extra}, which {COLLECTIONS} doesn't list.")

    if errors:
        errors.append("Run `python3 app/flatpak/pins.py update` after changing a pin.")
    return errors


def fetch_json(url: str) -> dict:
    with urllib.request.urlopen(url, timeout=60) as response:
        return json.load(response)


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def resolve_wheels(root: Path, python: str) -> list[Source]:
    """Every wheel the pins need, from PyPI, checked against what pip fetched."""
    with tempfile.TemporaryDirectory() as tmp:
        platforms = [arg for p in WHEEL_PLATFORMS for arg in ("--platform", p)]
        subprocess.run(
            [sys.executable, "-m", "pip", "download", "--quiet", "--dest", tmp,
             "--only-binary=:all:", "--implementation", "cp",
             "--python-version", python, "--abi", "cp" + python.replace(".", ""),
             *platforms, "-r", str(root / REQUIREMENTS)],
            check=True,
        )
        sources = []
        for wheel in sorted(Path(tmp).glob("*.whl")):
            name, version, _ = wheel_parts(wheel.name)
            release = fetch_json(f"https://pypi.org/pypi/{name}/{version}/json")
            match = [u for u in release["urls"] if u["filename"] == wheel.name]
            if not match:
                raise SystemExit(f"{wheel.name} isn't on PyPI's {name} {version} page")
            digest = match[0]["digests"]["sha256"]
            if digest != sha256(wheel):
                raise SystemExit(f"{wheel.name}: PyPI's sha256 doesn't match the download")
            sources.append(Source(wheel.name, match[0]["url"], digest))
        return sources


def resolve_collections(root: Path) -> list[Source]:
    sources = []
    for name, version in collection_pins(root).items():
        namespace, collection = name.split(".", 1)
        info = fetch_json(f"{GALAXY}/{namespace}/{collection}/versions/{version}/")
        artifact = info["artifact"]
        sources.append(Source(artifact["filename"], info["download_url"], artifact["sha256"]))
    return sources


def render(python: str, wheels: list[Source], collections: list[Source]) -> str:
    q = json.dumps  # a JSON string is a valid YAML double-quoted scalar
    site_packages = f"${{FLATPAK_DEST}}/lib/python{python}/site-packages"
    tarballs = " ".join(s.filename for s in collections)
    lines = [
        f"# Generated by `python3 app/flatpak/pins.py update` from {REQUIREMENTS}",
        f"# and {COLLECTIONS}. Don't edit by hand.",
        "#",
        "# Ansible and its WinRM client from wheels, then the Windows collections",
        "# into site-packages, where Ansible finds them without any config.",
        "name: ansible",
        "buildsystem: simple",
        "build-commands:",
        # --ignore-installed: the SDK has Python packages the Platform lacks,
        # so everything Ansible needs has to land in /app. pip resolves the
        # dependencies among these wheels only, so a missing one fails here.
        "  - " + q("pip3 install --no-index --find-links=. --ignore-installed"
                   " --no-warn-script-location --prefix=${FLATPAK_DEST} *.whl"),
        "  - " + q(f"ANSIBLE_HOME=$PWD/.ansible ansible-galaxy collection install --offline"
                   f" -p {site_packages} {tarballs}"),
        # ansible-test is for developing collections, not running playbooks.
        "cleanup:",
        "  - " + q("/bin/ansible-test"),
        "  - " + q(f"/lib/python{python}/site-packages/ansible_test"),
        "sources:",
    ]
    for source in wheels + collections:
        lines += [
            "  - type: file",
            f"    url: {q(source.url)}",
            f"    sha256: {source.sha256}",
        ]
    return "\n".join(lines) + "\n"


def update(root: Path) -> None:
    runtime = runtime_version(root)
    python = RUNTIME_PYTHON.get(runtime)
    if python is None:
        raise SystemExit(f"Runtime {runtime} isn't in RUNTIME_PYTHON; add its Python first.")
    text = render(python, resolve_wheels(root, python), resolve_collections(root))
    (root / ANSIBLE_MODULE).write_text(text, encoding="utf-8")
    print(f"Wrote {ANSIBLE_MODULE}.")


def main(argv: list[str]) -> int:
    if argv == ["update"]:
        update(ROOT)
        return 0
    if argv == ["check"]:
        errors = check(ROOT)
        for error in errors:
            print(error, file=sys.stderr)
        if not errors:
            print("Flatpak pins match the repo.")
        return 1 if errors else 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
