#!/usr/bin/env python3
"""Checks that a release's version is the same everywhere it's declared.

    python3 app/flatpak/release.py check                # CI runs it on every PR
    python3 app/flatpak/release.py check --tag v0.0.1   # the Release workflow

The app's version is in app/pubspec.yaml (before the +build number), the
crates' Cargo.toml and Cargo.lock, and the newest <release> in the metainfo,
which is what software centres show. `check` fails unless they all agree and
docs/releases/<version>.md (the release notes) exists; with --tag, also
unless the tag is v<version>. Then it prints `version=` and `notes=` lines,
the format of $GITHUB_OUTPUT. Only stdlib, so it runs on a bare CI runner.
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PUBSPEC = "app/pubspec.yaml"
CRATES = ("app/core/Cargo.toml", "app/rust/Cargo.toml")
CARGO_LOCK = "app/Cargo.lock"
METAINFO = "app/flatpak/io.github.sergimn.rega.metainfo.xml"
NOTES = "docs/releases/{version}.md"

# Plain MAJOR.MINOR.PATCH: the tag, the metainfo and Cargo all read it the same.
VERSION = re.compile(r"\d+\.\d+\.\d+")


def declared(root: Path) -> dict[str, str | None]:
    """The version each place declares, None where it declares none."""
    found: dict[str, str | None] = {}

    match = re.search(r"^version:\s*([^\s#]+)", (root / PUBSPEC).read_text(), re.M)
    found[PUBSPEC] = match and match.group(1).split("+")[0].strip("'\"")

    names = []
    for path in CRATES:
        package = tomllib.loads((root / path).read_text())["package"]
        names.append(package["name"])
        version = package.get("version")
        found[path] = version if isinstance(version, str) else None

    lock = tomllib.loads((root / CARGO_LOCK).read_text())
    locked = {p["name"]: p["version"] for p in lock.get("package", []) if "source" not in p}
    for name in names:
        found[f"{CARGO_LOCK} ({name})"] = locked.get(name)

    newest = ET.parse(root / METAINFO).getroot().find("releases/release")
    found[f"{METAINFO} (newest release)"] = None if newest is None else newest.get("version")
    return found


def check(root: Path, tag: str | None = None) -> tuple[str | None, list[str]]:
    """The version, and what's wrong with it; the version is None when they disagree."""
    found = declared(root)
    errors = [f"{place} declares no version" for place, v in found.items() if v is None]
    versions = {v for v in found.values() if v is not None}
    if len(versions) > 1:
        listed = "\n".join(f"  {place}: {v}" for place, v in found.items() if v is not None)
        errors.append(f"the app's version differs between files; set them all to the same one:\n{listed}")
    if errors or not versions:
        return None, errors

    [version] = versions
    if not VERSION.fullmatch(version):
        errors.append(f"version {version} isn't MAJOR.MINOR.PATCH")
    notes = root / NOTES.format(version=version)
    if not notes.is_file() or not notes.read_text().strip():
        errors.append(f"{NOTES.format(version=version)} is missing or empty; write the release notes there")
    if tag is not None and tag != f"v{version}":
        errors.append(f"tag {tag} doesn't match the app's version {version}; tag v{version}, or change the version")
    return version, errors


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["check"])
    parser.add_argument("--tag", help="the git tag being released, such as v0.0.1")
    args = parser.parse_args(argv)

    version, errors = check(ROOT, args.tag)
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    if errors:
        return 1
    print(f"version={version}")
    print(f"notes={NOTES.format(version=version)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
