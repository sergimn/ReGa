#!/usr/bin/env python3
"""Sources for building the Flatpak from source, offline, as Flathub does.

    python3 app/flatpak/sources.py update   # regenerate flutter.yml and rust.yml (needs network)
    python3 app/flatpak/sources.py write    # write generated/ from the locks; before flatpak-builder
    python3 app/flatpak/sources.py check    # offline; CI runs it

flatpak-builder downloads every source first, checks each against its
sha256, and then builds with no network. So everything the build needs is a
source with a hash, and none of it picks a version:

- flutter.yml, from `update`: the Flutter SDK CI uses (FLUTTER_VERSION in
  .github/workflows/app.yml) at its tag's commit, what its tool downloads
  for a Linux x86_64 release build (the Dart SDK, the engine, the fonts),
  and the Dart packages the tool builds itself from, which flutter_tools
  pins to exact versions.
- rust.yml, from `update`: the Rust toolchain CI uses (RUST_VERSION), with
  the hashes in the release's channel manifest.
- generated/cargo-sources.json and generated/pub-sources.json, from
  `write`: every crate in app/Cargo.lock, and every Dart package in
  app/pubspec.lock and in the pubspec.lock of cargokit's build tool, with
  the hash the lock has for it. They're written for each build rather than
  committed, so Renovate's updates of those locks need nothing by hand.

`check` fails when flutter.yml or rust.yml is for another version than CI
uses, or when a lock has something `write` can't make an offline source of
(a git dependency, say). Only stdlib (and git, for `update`), so it runs on
a bare CI runner.
"""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
import sys
import tomllib
import urllib.request
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "app/flatpak/io.github.sergimn.rega.yml"
FLUTTER_MODULE = "app/flatpak/flutter.yml"
RUST_MODULE = "app/flatpak/rust.yml"
FLUTTER_PATCH = "app/flatpak/patches/flutter-offline.patch"
GENERATED = "app/flatpak/generated"
CARGO_SOURCES = f"{GENERATED}/cargo-sources.json"
PUB_SOURCES = f"{GENERATED}/pub-sources.json"
WORKFLOW = ".github/workflows/app.yml"
CARGO_LOCK = "app/Cargo.lock"
PUB_LOCKS = ("app/pubspec.lock", "app/rust_builder/cargokit/build_tool/pubspec.lock")

# The manifest only builds for x86_64, like Terraform in it.
ARCH = "x86_64"
FLUTTER_ARCH = "x64"
RUST_TARGET = f"{ARCH}-unknown-linux-gnu"
RUST_COMPONENTS = ("rustc", "cargo", "rust-std")

FLUTTER_GIT = "https://github.com/flutter/flutter.git"
FLUTTER_RAW = "https://raw.githubusercontent.com/flutter/flutter"
STORAGE = "https://storage.googleapis.com"
PUB_DEV = "https://pub.dev"
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
RUST_DIST = "https://static.rust-lang.org/dist"

# Where the sources go in the build. The pub cache and cargo's vendored
# crates are in the module's own folder (/run/build/<module>), which the
# manifest points PUB_CACHE and CARGO_HOME at.
PUB_CACHE = "pub-cache"
CARGO_VENDOR = "cargo/vendor"
CARGO_CONFIG = f"""\
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "{CARGO_VENDOR}"
"""


@dataclass(frozen=True)
class Download:
    url: str
    # Where flatpak-builder unpacks (an archive) or puts (a file) it, in the
    # module's folder: where the Flutter tool would have put it.
    dest: str
    archive: bool = True
    # 1 for a zip of one folder (and its licence) that goes where another
    # zip of the same kind goes, whose licence has the same name.
    strip: int = 0


def flutter_downloads(engine: str, material_fonts: str, gradle_wrapper: str) -> list[Download]:
    """What the Flutter tool downloads for `flutter build linux --release`
    on x86_64 (see Cache in flutter_tools/lib/src/flutter_cache.dart), and
    where it unpacks each one. Most zips hold a folder and a licence."""
    e = f"{STORAGE}/flutter_infra_release/flutter/{engine}"
    cache = "flutter/bin/cache"
    engines = f"{cache}/artifacts/engine"
    a = FLUTTER_ARCH
    return [
        Download(f"{e}/dart-sdk-linux-{a}.zip", cache),
        Download(f"{STORAGE}/{material_fonts}", f"{cache}/artifacts/material_fonts"),
        # Only Android builds use it, but the tool wants it for any build.
        Download(f"{STORAGE}/{gradle_wrapper}", f"{cache}/artifacts/gradle_wrapper"),
        Download(f"{e}/engine_stamp.json", cache, archive=False),
        Download(f"{e}/sky_engine.zip", f"{cache}/pkg/sky_engine", strip=1),
        Download(f"{e}/flutter_gpu.zip", f"{cache}/pkg/flutter_gpu", strip=1),
        Download(f"{e}/flutter_patched_sdk.zip", f"{engines}/common/flutter_patched_sdk", strip=1),
        Download(f"{e}/flutter_patched_sdk_product.zip", f"{engines}/common/flutter_patched_sdk_product",
                 strip=1),
        Download(f"{e}/linux-{a}/artifacts.zip", f"{engines}/linux-{a}"),
        Download(f"{e}/linux-{a}/font-subset.zip", f"{engines}/linux-{a}"),
        # The tool counts the Linux engine as there once these two are; a
        # release build only uses the release one. The debug engine, which
        # would go in linux-x64 with artifacts.zip, isn't needed.
        Download(f"{e}/linux-{a}-profile/linux-{a}-flutter-gtk.zip", f"{engines}/linux-{a}-profile"),
        Download(f"{e}/linux-{a}-release/linux-{a}-flutter-gtk.zip", f"{engines}/linux-{a}-release"),
    ]


# The stamps the tool writes after downloading each of those, with the
# bin/internal file whose contents it writes (the engine's version for the
# engine's parts). With them there, it doesn't try to download anything.
FLUTTER_STAMPS = {
    "engine-dart-sdk": "engine.version",
    "engine_stamp": "engine.version",
    "flutter_sdk": "engine.version",
    "font-subset": "engine.version",
    "linux-sdk": "engine.version",
    "material_fonts": "material_fonts.version",
    "gradle_wrapper": "gradle_wrapper.version",
}


def read(root: Path, path: str) -> str:
    return (root / path).read_text(encoding="utf-8")


def workflow_version(root: Path, name: str) -> str:
    m = re.search(rf"^\s*{name}:\s*['\"]?([^'\"\s]+)", read(root, WORKFLOW), re.M)
    if not m:
        raise SystemExit(f"{WORKFLOW}: no {name}")
    return m.group(1)


def scalar(value: str) -> str:
    value = value.strip()
    return json.loads(value) if value.startswith('"') else value


def parse_pub_lock(text: str, path: str) -> dict[str, dict]:
    """The packages of a pubspec.lock, as pub writes it: by name, each a map
    of its fields, with `description` a map of its own (or a string)."""
    packages: dict[str, dict] = {}
    section = name = field = None
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = len(line) - len(line.lstrip(" "))
        key, colon, value = line.strip().partition(":")
        if not colon:
            raise SystemExit(f"{path}: expected `key: value`, not {line.strip()!r}")
        key = scalar(key)
        if indent == 0:
            section = key
        elif section != "packages":
            continue
        elif indent == 2:
            name = key
            packages[name] = {}
        elif indent == 4 and name:
            field = key
            packages[name][key] = scalar(value) if value.strip() else {}
        elif indent == 6 and name and isinstance(packages[name].get(field), dict):
            packages[name][field][key] = scalar(value)
        else:
            raise SystemExit(f"{path}: can't read {line.strip()!r}")
    return packages


def pub_package(name: str, version: str, sha256: str) -> list[dict]:
    """A package in the pub cache, as `pub get` would leave it."""
    return [
        {
            "type": "archive",
            "archive-type": "tar-gzip",
            "url": f"{PUB_DEV}/api/archives/{name}-{version}.tar.gz",
            "sha256": sha256,
            "strip-components": 0,
            "dest": f"{PUB_CACHE}/hosted/pub.dev/{name}-{version}",
        },
        {
            "type": "inline",
            "contents": sha256,
            "dest": f"{PUB_CACHE}/hosted-hashes/pub.dev",
            "dest-filename": f"{name}-{version}.sha256",
        },
    ]


def pub_sources(root: Path) -> tuple[list[dict], list[str]]:
    """Every package the pub locks get from pub.dev, once. The Flutter SDK's
    own (`source: sdk`) and the app's (`source: path`) need no source."""
    sources, errors, seen = [], [], set()
    for path in PUB_LOCKS:
        for name, package in parse_pub_lock(read(root, path), path).items():
            source = package.get("source")
            if source in ("sdk", "path"):
                continue
            description = package.get("description")
            if source != "hosted" or not isinstance(description, dict) or description.get("url") != PUB_DEV:
                errors.append(f"{path}: {name} comes from {source} {description}, not pub.dev, so it has no "
                              "offline source; teach sources.py where to get it.")
                continue
            version, sha256 = package.get("version", ""), description.get("sha256", "")
            if not re.fullmatch(r"[0-9a-f]{64}", sha256):
                errors.append(f"{path}: {name} {version} has no sha256; run `flutter pub get` to add it.")
                continue
            if (name, version) not in seen:
                seen.add((name, version))
                sources += pub_package(name, version, sha256)
    return sources, errors


def cargo_sources(root: Path) -> tuple[list[dict], list[str]]:
    """Every crate in Cargo.lock, vendored the way `cargo vendor` would, and
    the config that has cargo use them instead of crates.io."""
    sources, errors = [], []
    for package in tomllib.loads(read(root, CARGO_LOCK)).get("package", []):
        name, version, source = package["name"], package["version"], package.get("source")
        if source is None:
            continue  # the app's own
        checksum = package.get("checksum", "")
        if source != CRATES_IO or not re.fullmatch(r"[0-9a-f]{64}", checksum):
            errors.append(f"{CARGO_LOCK}: {name} {version} comes from {source}, not crates.io with a checksum, "
                          "so it has no offline source; teach sources.py where to get it.")
            continue
        dest = f"{CARGO_VENDOR}/{name}-{version}"
        sources += [
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                "sha256": checksum,
                "dest": dest,
            },
            {
                # cargo checks the crate's checksum against Cargo.lock, and
                # takes the files as they are.
                "type": "inline",
                "contents": json.dumps({"package": checksum, "files": {}}),
                "dest": dest,
                "dest-filename": ".cargo-checksum.json",
            },
        ]
    sources.append({"type": "inline", "contents": CARGO_CONFIG, "dest": "cargo", "dest-filename": "config.toml"})
    return sources, errors


def write(root: Path) -> list[str]:
    cargo, cargo_errors = cargo_sources(root)
    pub, pub_errors = pub_sources(root)
    if cargo_errors or pub_errors:
        return cargo_errors + pub_errors
    (root / GENERATED).mkdir(parents=True, exist_ok=True)
    for path, sources in ((CARGO_SOURCES, cargo), (PUB_SOURCES, pub)):
        (root / path).write_text(json.dumps(sources, indent=2) + "\n", encoding="utf-8")
    crates = sum(1 for s in cargo if s["type"] == "archive")
    packages = sum(1 for s in pub if s["type"] == "archive")
    print(f"Wrote the sources of {crates} crates to {CARGO_SOURCES} and {packages} Dart packages to {PUB_SOURCES}.")
    return []


def module_tag(text: str) -> str | None:
    m = re.search(r"^\s*tag:\s*\"?([^\s\"]+)\"?\s*$", text, re.M)
    return m.group(1) if m else None


def module_rust_versions(text: str) -> set[str]:
    return set(re.findall(rf"/(?:{'|'.join(RUST_COMPONENTS)})-([^/\s\"]+)-{RUST_TARGET}\.tar\.xz", text))


def unhashed(text: str) -> list[str]:
    """The URLs in a generated module's sources that have no sha256 (or, for
    git, no commit) to check what's downloaded against."""
    missing = []
    for source in re.split(r"^  - ", text.split("\nsources:\n", 1)[-1], flags=re.M):
        url = re.search(r"^\s*url:\s*\"?([^\s\"]+)", source, re.M)
        if url and not re.search(r"^\s*(sha256:\s*\"?[0-9a-f]{64}|commit:\s*\"?[0-9a-f]{40})\"?\s*$", source, re.M):
            missing.append(url.group(1))
    return missing


def flutter_mismatches(text: str) -> list[str]:
    """flutter.yml's build checks that the commit's engine, fonts and Gradle
    wrapper are the versions it names; this checks its downloads are those."""
    internal = {name: version for version, name in
                re.findall(r"^\s*- grep -qxF (\S+) flutter/bin/internal/(\w+)\.version$", text, re.M)}
    if set(internal) != {"engine", "material_fonts", "gradle_wrapper"}:
        return [f"{FLUTTER_MODULE} doesn't check the versions in Flutter's bin/internal."]
    errors = []
    engines = set(re.findall(r"/flutter_infra_release/flutter/([0-9a-f]{40})/", text))
    if engines != {internal["engine"]}:
        errors.append(f"{FLUTTER_MODULE} downloads engine {', '.join(sorted(engines)) or 'nothing'}, "
                      f"but checks for {internal['engine']}.")
    for name in ("material_fonts", "gradle_wrapper"):
        if f'"{STORAGE}/{internal[name]}"' not in text:
            errors.append(f"{FLUTTER_MODULE} doesn't download the {name} it checks for.")
    return errors


def check(root: Path) -> list[str]:
    """Every way the Flatpak's sources have drifted from what CI uses."""
    errors = []
    manifest = read(root, MANIFEST)
    for module in (FLUTTER_MODULE, RUST_MODULE):
        name = Path(module).name
        if not re.search(rf"^\s*-\s*\"?{re.escape(name)}\"?\s*$", manifest, re.M):
            errors.append(f"{MANIFEST} doesn't include {name} in its modules.")
        if not (root / module).exists():
            errors.append(f"{module} is missing; run sources.py update.")
            continue
        for url in unhashed(read(root, module)):
            errors.append(f"{module} has {url} with no sha256 after it.")
    if (root / FLUTTER_MODULE).exists():
        errors += flutter_mismatches(read(root, FLUTTER_MODULE))
    for generated in (CARGO_SOURCES, PUB_SOURCES):
        if not re.search(rf"^\s*-\s*\"?{re.escape(Path(generated).relative_to(Path(MANIFEST).parent).as_posix())}"
                         r"\"?\s*$", manifest, re.M):
            errors.append(f"{MANIFEST} doesn't use {generated} in its sources.")

    drift = []
    flutter = workflow_version(root, "FLUTTER_VERSION")
    if (root / FLUTTER_MODULE).exists() and (tag := module_tag(read(root, FLUTTER_MODULE))) != flutter:
        drift.append(f"{FLUTTER_MODULE} has Flutter {tag}, but CI builds with {flutter}.")
    rust = workflow_version(root, "RUST_VERSION")
    if (root / RUST_MODULE).exists() and (versions := module_rust_versions(read(root, RUST_MODULE))) != {rust}:
        drift.append(f"{RUST_MODULE} has Rust {', '.join(sorted(versions)) or 'nothing'}, but CI builds with {rust}.")
    if drift:
        errors += drift
        errors.append("Run `python3 app/flatpak/sources.py update` after FLUTTER_VERSION or RUST_VERSION changes.")

    errors += cargo_sources(root)[1] + pub_sources(root)[1]
    return errors


# `update`: everything below needs the network.


def fetch(url: str) -> bytes:
    with urllib.request.urlopen(url, timeout=120) as response:
        return response.read()


def sha256_of(url: str) -> str:
    print(f"Hashing {url}", file=sys.stderr)
    digest = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=120) as response:
        while chunk := response.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def tag_commit(tag: str) -> str:
    """The commit a Flutter release tag points at."""
    out = subprocess.run(["git", "ls-remote", FLUTTER_GIT, f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"],
                         capture_output=True, text=True, check=True).stdout
    refs = {ref: sha for sha, ref in (line.split("\t") for line in out.splitlines())}
    commit = refs.get(f"refs/tags/{tag}^{{}}") or refs.get(f"refs/tags/{tag}")
    if not commit:
        raise SystemExit(f"Flutter has no tag {tag}.")
    return commit


def pubspec_pins(text: str, path: str) -> dict[str, str]:
    """`dependencies` and `dev_dependencies` of a pubspec that pins each one
    exactly, as flutter_tools does: name to version."""
    pins, section = {}, None
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if not line.startswith(" "):
            section = line.split(":", 1)[0]
        elif section in ("dependencies", "dev_dependencies") and re.match(r"  \S", line):
            m = re.fullmatch(r"  ([a-z0-9_]+):\s*([0-9][^\s#]*)\s*(#.*)?", line)
            if not m:
                raise SystemExit(f"{path}: expected an exact version, not {line.strip()!r}")
            pins[m.group(1)] = m.group(2)
    return pins


def flutter_tool_packages(commit: str) -> list[tuple[str, str, str]]:
    """The packages `pub upgrade` gets for flutter_tools, as (name, version,
    sha256). It pins most of them to exact versions. For the few it leaves
    to the resolver, this takes the version Flutter's own pubspec.lock (the
    workspace's, at the same commit) has, rather than whatever is newest."""
    path = "packages/flutter_tools/pubspec.yaml"
    pins = pubspec_pins(fetch(f"{FLUTTER_RAW}/{commit}/{path}").decode(), path)
    workspace = parse_pub_lock(fetch(f"{FLUTTER_RAW}/{commit}/pubspec.lock").decode(), "pubspec.lock")
    chosen = dict(pins)
    queue = sorted(pins)
    packages = []
    while queue:
        name = queue.pop(0)
        info = json.loads(fetch(f"{PUB_DEV}/api/packages/{name}/versions/{chosen[name]}"))
        packages.append((name, chosen[name], info["archive_sha256"]))
        for dependency, constraint in sorted((info["pubspec"].get("dependencies") or {}).items()):
            if dependency in chosen or (isinstance(constraint, dict) and "sdk" in constraint):
                continue
            locked = workspace.get(dependency, {})
            if locked.get("source") != "hosted":
                raise SystemExit(f"flutter_tools at {commit} doesn't pin {dependency}, which {name} needs, "
                                 "and Flutter's pubspec.lock doesn't have it from pub.dev either.")
            chosen[dependency] = locked["version"]
            queue.append(dependency)
    return sorted(packages)


def patch_misses(patch: str, text: str) -> list[str]:
    """The lines a patch removes that the file it patches doesn't have."""
    lines = set(text.splitlines())
    hunks = patch[patch.index("\n@@") :] if "\n@@" in patch else ""
    return [line[1:] for line in hunks.splitlines()
            if line.startswith("-") and not line.startswith("---") and line[1:] not in lines]


def render_flutter(tag: str, commit: str, internal: dict[str, str], downloads: list[tuple[Download, str]],
                   packages: list[tuple[str, str, str]]) -> str:
    q = json.dumps  # a JSON string is a valid YAML double-quoted scalar
    lines = [
        "# Generated by `python3 app/flatpak/sources.py update` from FLUTTER_VERSION",
        f"# in {WORKFLOW}. Don't edit by hand.",
        "#",
        "# The Flutter SDK, with what its tool would download for a Linux release",
        "# build and the Dart packages the tool is built from, in /var/lib, which",
        "# the build of the app uses and the Flatpak doesn't ship.",
        "name: flutter",
        "buildsystem: simple",
        "build-commands:",
    ]
    # The downloads below are for these versions; stop if the commit's differ.
    lines += [f"  - grep -qxF {version} flutter/bin/internal/{name}.version" for name, version in internal.items()]
    # What the tool writes once it has downloaded each part.
    lines += [f"  - cp flutter/bin/internal/{source} flutter/bin/cache/{stamp}.stamp"
              for stamp, source in FLUTTER_STAMPS.items()]
    lines += [
        "  - mkdir -p /var/lib",
        "  - cp -a flutter /var/lib/flutter",
        f"  - cp -a {PUB_CACHE} /var/lib/flutter-pub-cache",
        # Builds the tool now, offline (see patches/flutter-offline.patch).
        "  - PUB_CACHE=/var/lib/flutter-pub-cache /var/lib/flutter/bin/flutter --version",
        "sources:",
        "  - type: git",
        f"    url: {q(FLUTTER_GIT)}",
        f"    tag: {q(tag)}",
        f"    commit: {commit}",
        "    dest: flutter",
        "  - type: patch",
        "    path: patches/flutter-offline.patch",
    ]
    for download, sha256 in downloads:
        lines += [
            f"  - type: {'archive' if download.archive else 'file'}",
            f"    url: {q(download.url)}",
            f"    sha256: {sha256}",
        ]
        if download.archive:
            lines.append(f"    strip-components: {download.strip}")
        lines.append(f"    dest: {download.dest}")
    for name, version, sha256 in packages:
        for source in pub_package(name, version, sha256):
            lines.append(f"  - type: {source['type']}")
            lines += [f"    {key}: {value if key == 'sha256' or not isinstance(value, str) else q(value)}"
                      for key, value in source.items() if key != "type"]
    return "\n".join(lines) + "\n"


def render_rust(version: str, components: list[tuple[str, str, str]]) -> str:
    q = json.dumps
    names = " ".join(name for name, _, _ in components)
    lines = [
        "# Generated by `python3 app/flatpak/sources.py update` from RUST_VERSION",
        f"# in {WORKFLOW}. Don't edit by hand.",
        "#",
        f"# Rust {version} for {RUST_TARGET}, in /var/lib/rust, which the",
        "# build of the app uses and the Flatpak doesn't ship, with rustup.sh as",
        "# its rustup.",
        "name: rust",
        "buildsystem: simple",
        "build-commands:",
        "  - " + q(f"for c in {names}; do $c/install.sh --prefix=/var/lib/rust --disable-ldconfig; done"),
        "  - install -Dm755 rustup.sh /var/lib/rust/bin/rustup",
        "sources:",
    ]
    for name, url, sha256 in components:
        lines += [
            "  - type: archive",
            f"    url: {q(url)}",
            f"    sha256: {sha256}",
            f"    dest: {name}",
        ]
    lines += ["  - type: file", "    path: rustup.sh"]
    return "\n".join(lines) + "\n"


def rust_components(version: str) -> list[tuple[str, str, str]]:
    """Each component's URL and sha256, from the release's channel manifest."""
    channel = tomllib.loads(fetch(f"{RUST_DIST}/channel-rust-{version}.toml").decode())
    components = []
    for name in RUST_COMPONENTS:
        package = channel["pkg"][name]
        target = package["target"].get(RUST_TARGET, {})
        if not target.get("available") or f"-{version}-" not in target.get("xz_url", ""):
            raise SystemExit(f"Rust {version} has no {name} for {RUST_TARGET}.")
        components.append((name, target["xz_url"], target["xz_hash"]))
    return components


def update(root: Path) -> None:
    tag = workflow_version(root, "FLUTTER_VERSION")
    commit = tag_commit(tag)
    shared = fetch(f"{FLUTTER_RAW}/{commit}/bin/internal/shared.sh").decode()
    if stale := patch_misses(read(root, FLUTTER_PATCH), shared):
        raise SystemExit(f"{FLUTTER_PATCH} no longer applies to Flutter {tag}'s bin/internal/shared.sh, "
                         f"which doesn't have: {stale[0]!r}. Update the patch.")
    internal = {name: fetch(f"{FLUTTER_RAW}/{commit}/bin/internal/{name}.version").decode().strip()
                for name in ("engine", "material_fonts", "gradle_wrapper")}
    downloads = [(d, sha256_of(d.url)) for d in flutter_downloads(
        internal["engine"], internal["material_fonts"], internal["gradle_wrapper"])]
    text = render_flutter(tag, commit, internal, downloads, flutter_tool_packages(commit))
    (root / FLUTTER_MODULE).write_text(text, encoding="utf-8")
    print(f"Wrote {FLUTTER_MODULE} for Flutter {tag}.")

    rust = workflow_version(root, "RUST_VERSION")
    (root / RUST_MODULE).write_text(render_rust(rust, rust_components(rust)), encoding="utf-8")
    print(f"Wrote {RUST_MODULE} for Rust {rust}.")


def main(argv: list[str]) -> int:
    if argv == ["update"]:
        update(ROOT)
        return 0
    if argv in (["check"], ["write"]):
        errors = check(ROOT) if argv == ["check"] else write(ROOT)
        for error in errors:
            print(error, file=sys.stderr)
        if argv == ["check"] and not errors:
            print("The Flatpak's sources match what CI builds with.")
        return 1 if errors else 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
