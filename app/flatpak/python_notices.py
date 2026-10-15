#!/usr/bin/env python3
"""The licence notices of the Python packages the Flatpak bundles.

    python3 app/flatpak/python_notices.py write SITE_PACKAGES DIR

The Flatpak runs Ansible on its own copy of Ansible, its WinRM client and
their dependencies (the wheels ansible.yml installs, from
infra/ansible/requirements.lock), and the Windows collections (from
infra/ansible/requirements.yml). Their licences (GPL for Ansible, MIT,
BSD, Apache-2.0 and the like for the rest) ask for their notices and
licence texts to go with every copy, so `write` reads what is installed in
SITE_PACKAGES and puts python-packages.txt in DIR: the packages and
collections with their versions and licences, then the licence texts each
one ships, each text once, under the names of those that ship it. The
layout is rust-crates.txt's (rust_notices.py), which the app's licence page
reads.

It fails, saying what to do, when what is installed isn't exactly what the
lock and requirements.yml pin for the runtime (so the list can't go stale
after a Renovate update), when a package or collection ships no licence
text or leaves out one its metadata names, or when a collection names no licence and isn't in LICENSE_FILE_ONLY.
The Flatpak build runs it after installing Ansible. Only stdlib.
"""

from __future__ import annotations

import hashlib
import json
import sys
from dataclasses import dataclass
from email.parser import BytesHeaderParser
from pathlib import Path

import pins
from rust_notices import LICENSE_NAME, RULE, THIN_RULE, Text, normalize as normalize_text

ROOT = Path(__file__).resolve().parents[2]
PACKAGES_FILE = "python-packages.txt"
WHERE = "app/flatpak/python_notices.py"

# Collections whose MANIFEST.json names no licence, only a licence file:
# the licence someone read in that file (and the collection's README), and
# the file's sha256, so a change to it fails until someone reads it again.
LICENSE_FILE_ONLY = {
    "ansible.windows": ("GPL-3.0-or-later", "0ae0485a5bd37a63e63603596417e4eb0e653334fa6c7f932ca3a0e85d4af227"),
    "community.windows": ("GPL-3.0-or-later", "0ae0485a5bd37a63e63603596417e4eb0e653334fa6c7f932ca3a0e85d4af227"),
}

# Trove classifiers name a licence after this.
CLASSIFIER = "License :: "


@dataclass(frozen=True)
class Package:
    # As its metadata spells it (PyYAML), or namespace.name for a collection.
    name: str
    version: str
    license: str
    texts: tuple[Text, ...]
    collection: bool = False

    def __str__(self) -> str:
        return f"{self.name} {self.version}"


def licence_of(metadata) -> str:
    """A package's licence as its metadata gives it: the SPDX
    License-Expression, else the License field when it is a short name
    (some packages put a copyright line there), else the licence
    classifiers."""
    if expression := (metadata.get("License-Expression") or "").strip():
        return expression
    field = (metadata.get("License") or "").strip()
    if (field and "\n" not in field and len(field) <= 40
            and not field.lower().startswith(("copyright", "see ", "unknown"))):
        return field
    classifiers = [c.rsplit(" :: ", 1)[-1] for c in metadata.get_all("Classifier") or []
                   if c.startswith(CLASSIFIER) and c != f"{CLASSIFIER}OSI Approved"]
    return " OR ".join(classifiers) or "unknown"


def dist_texts(dist_info: Path, metadata) -> tuple[list[Text], list[str]]:
    """The licence files a .dist-info folder has: those its License-File
    lines name (in licenses/ since metadata 2.4, at the top before), else
    any with a licence's name; then the named ones it doesn't have."""
    texts = []
    missing = []
    for name in metadata.get_all("License-File") or []:
        for path in (dist_info / "licenses" / name, dist_info / name):
            if path.is_file():
                texts.append(Text(path.relative_to(dist_info).as_posix(), normalize_text(path.read_bytes())))
                break
        else:
            missing.append(name)
    if not texts:
        for path in sorted(dist_info.rglob("*")):
            relative = path.relative_to(dist_info)
            if path.is_file() and (relative.parts[0] == "licenses" or LICENSE_NAME.fullmatch(path.name)):
                texts.append(Text(relative.as_posix(), normalize_text(path.read_bytes())))
    return texts, missing


def installed(site_packages: Path) -> tuple[list[Package], list[str]]:
    """The packages and collections installed in site_packages, with their
    licence texts; then what's wrong with them."""
    packages = []
    errors = []
    for dist_info in sorted(site_packages.glob("*.dist-info")):
        metadata = BytesHeaderParser().parsebytes((dist_info / "METADATA").read_bytes())
        texts, missing = dist_texts(dist_info, metadata)
        package = Package(metadata["Name"], metadata["Version"], licence_of(metadata), tuple(texts))
        for name in missing:
            errors.append(f"{package}'s metadata names the licence file {name}, which isn't in "
                          f"{dist_info.name}. Find out where it is and how to ship it, and teach {WHERE} to.")
        if not package.texts:
            errors.append(f"{package} ships no licence text in {dist_info.name}. Find out where its "
                          f"licence is and how to ship it, and teach {WHERE} to.")
        packages.append(package)

    for manifest in sorted(site_packages.glob("ansible_collections/*/*/MANIFEST.json")):
        info = json.loads(manifest.read_text(encoding="utf-8"))["collection_info"]
        folder = manifest.parent
        name = f"{info['namespace']}.{info['name']}"
        at = f"{name} {info['version']}"
        files = sorted(p for p in folder.iterdir() if p.is_file() and LICENSE_NAME.fullmatch(p.name))
        license_file = folder / info["license_file"] if info.get("license_file") else None
        if license_file and license_file.is_file() and license_file not in files:
            files.insert(0, license_file)
        license = " OR ".join(info.get("license") or [])
        if not license:
            sha256 = hashlib.sha256(license_file.read_bytes()).hexdigest() if license_file and license_file.is_file() else None
            if name not in LICENSE_FILE_ONLY:
                only = f"only the file {info['license_file']}" if info.get("license_file") else "nor a licence file"
                errors.append(f"{at} names no licence in its MANIFEST.json, {only}. "
                              f"Read it, and add the collection to LICENSE_FILE_ONLY in {WHERE}, with its "
                              f"licence and the file's sha256 ({sha256}).")
            elif LICENSE_FILE_ONLY[name][1] != sha256:
                errors.append(f"{at}: its licence file {info.get('license_file')} isn't the one read as "
                              f"{LICENSE_FILE_ONLY[name][0]}. Read it, then set its licence and sha256 ({sha256}) "
                              f"in LICENSE_FILE_ONLY in {WHERE}.")
            license = LICENSE_FILE_ONLY.get(name, ("unknown",))[0]
        texts = tuple(Text(p.relative_to(folder).as_posix(), normalize_text(p.read_bytes())) for p in files)
        if not texts:
            errors.append(f"{at} ships no licence text. Find out where its licence is and how to ship it, "
                          f"and teach {WHERE} to.")
        packages.append(Package(name, info["version"], license, texts, collection=True))
    return packages, errors


def pinned(root: Path, packages: list[Package]) -> list[str]:
    """How what's installed differs from what the repo pins for the runtime."""
    runtime_name = pins.runtime_version(root)
    runtime = pins.RUNTIMES.get(runtime_name)
    if runtime is None:
        return [f"Runtime {runtime_name} isn't in RUNTIMES in pins.py; add its Python and glibc."]
    errors = []
    have = {pins.normalize(p.name): p for p in packages if not p.collection}
    for name, locked in sorted(pins.locked_for(root, runtime).items()):
        if name not in have:
            errors.append(f"{pins.LOCK} has {name}=={locked.version}, which isn't installed.")
        elif not pins.same_version(have[name].version, locked.version):
            errors.append(f"{pins.LOCK} has {name}=={locked.version}, but {have[name]} is installed.")
    for name in sorted(have.keys() - pins.locked_for(root, runtime).keys()):
        errors.append(f"{have[name]} is installed, but {pins.LOCK} doesn't have it.")
    collections = {p.name: p for p in packages if p.collection}
    wanted = pins.collection_pins(root)
    for name, version in sorted(wanted.items()):
        if name not in collections:
            errors.append(f"{pins.COLLECTIONS} has {name} {version}, which isn't installed.")
        elif collections[name].version != version:
            errors.append(f"{pins.COLLECTIONS} has {name} {version}, but {collections[name]} is installed.")
    for name in sorted(collections.keys() - wanted.keys()):
        errors.append(f"{collections[name]} is installed, but {pins.COLLECTIONS} doesn't have it.")
    if errors:
        errors.append("The Flatpak's ansible.yml is out of step: run `python3 app/flatpak/pins.py update`.")
    return errors


def render(lock_sha256: str, packages: list[Package]) -> str:
    out = [
        "Licences of the Python packages in ReGa",
        RULE,
        "",
        "ReGa's Flatpak runs Ansible with the Python packages and Ansible",
        "collections below, at these versions. Their licence texts follow, as",
        "each one ships them. A text that several ship word for word is printed",
        "once, under all of their names.",
        "",
        f"Made by {WHERE} from {pins.LOCK}, sha256 {lock_sha256},",
        f"and {pins.COLLECTIONS}.",
        "",
    ]
    for heading, collection in (("Python packages", False), ("Ansible collections", True)):
        listed = [p for p in packages if p.collection == collection]
        width = max((len(str(p)) for p in listed), default=0)
        out += ["", heading, THIN_RULE, "", *(f"{str(p):<{width}}  {p.license}" for p in listed), ""]
    by_body: dict[str, list[str]] = {}
    for package in packages:
        for text in package.texts:
            by_body.setdefault(text.body, []).append(f"{package}: {text.source}")
    out += ["", "Licence texts", THIN_RULE]
    for body, sources in by_body.items():
        out += ["", RULE, *sources, THIN_RULE, "", body.rstrip("\n")]
    return "\n".join(out) + "\n"


def write(root: Path, site_packages: Path, out: Path) -> list[str]:
    """Writes python-packages.txt in out; the problems instead, if any."""
    packages, errors = installed(site_packages)
    if not packages:
        return [f"Nothing is installed in {site_packages}."]
    errors += pinned(root, packages)
    if errors:
        return errors
    packages.sort(key=lambda p: (p.collection, p.name.lower()))
    lock_sha256 = hashlib.sha256((root / pins.LOCK).read_bytes()).hexdigest()
    out.mkdir(parents=True, exist_ok=True)
    (out / PACKAGES_FILE).write_text(render(lock_sha256, packages), encoding="utf-8")
    print(f"Wrote the licence texts of {len(packages)} Python packages and Ansible collections to {out}/.")
    return []


def main(argv: list[str]) -> int:
    if len(argv) != 3 or argv[0] != "write":
        print(__doc__, file=sys.stderr)
        return 2
    errors = write(ROOT, Path(argv[1]), Path(argv[2]))
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
