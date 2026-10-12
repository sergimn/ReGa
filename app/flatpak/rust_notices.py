#!/usr/bin/env python3
"""The licence texts of the Rust crates built into the app, for the Flatpak.

    python3 app/flatpak/rust_notices.py write OUT   # the file the Flatpak ships
    python3 app/flatpak/rust_notices.py check       # CI runs it on every PR

The app's Rust core (rega_bridge, with rega_core) is built with the crates
in app/Cargo.lock. Their licences (MIT, Apache-2.0, BSD and the like) ask
that their copyright notices and licence texts go with every copy, so
`write` gathers the texts each crate ships into one file: the crates and
their versions, then each text once, under the names of the crates that
ship it. The crates are those `cargo tree` finds for the build the Flatpak
packages (cargokit's `cargo build -p rega_bridge` for x86_64 Linux): normal
and build dependencies, not dev-dependencies.

Both fail when a crate's licence isn't one that's met by shipping its
notice (ACCEPTED), or no text can be found for it; the message says what to
do. Nothing is checked in that a crate update would make stale, so Renovate's
crate updates need nothing by hand unless one of those happens.

Needs cargo, which downloads the crates in Cargo.lock if they aren't there
yet. Only stdlib otherwise.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKSPACE = "app/Cargo.toml"
LOCK = "app/Cargo.lock"
# Licence texts of crates that don't ship one, by crate (see UNSHIPPED).
TEXTS = "app/flatpak/rust-licenses"

# What the Flatpak packages: the Flutter bundle, whose Rust library cargokit
# builds with `cargo build -p rega_bridge --target x86_64-unknown-linux-gnu`.
PACKAGE = "rega_bridge"
TARGET = "x86_64-unknown-linux-gnu"

# Licences whose terms a binary meets by shipping the crate's notice and
# licence text, which is what this file is for. A crate under anything else
# (copyleft, say, or a licence that asks for more than a notice) fails the
# check until someone has read its terms and added it here, or dropped it.
ACCEPTED = frozenset({
    "0BSD",
    "Apache-2.0",
    "BlueOak-1.0.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "ISC",
    "MIT",
    "MIT-0",
    "Unicode-3.0",
    "Unlicense",
    "Zlib",
})

# Crates published without their licence text. Theirs is checked in under
# TEXTS/<crate>/, from the crate's repository at the commit the release was
# published from (`git.sha1` in the crate's .cargo_vcs_info.json). Each is
# listed with the `license` its Cargo.toml had then: when that changes, the
# check fails until someone takes the text again.
UNSHIPPED = {
    # github.com/Nugine/simd at d74c030 (0.8.0)
    "base64-simd": "MIT",
    "vsimd": "MIT",
    # github.com/cloudflare/boringtun at 051c9d4 (0.7.1)
    "boringtun": "BSD-3-Clause",
    # github.com/dart-sys/dart-sys at c932943 (4.1.5)
    "dart-sys": "MIT OR Apache-2.0",
    # github.com/fzyzcjy/flutter_rust_bridge at 8631db9 (2.13.0)
    "flutter_rust_bridge": "MIT",
    "flutter_rust_bridge_macros": "MIT",
}

# Crates whose Cargo.toml has only a `license-file`, no SPDX `license`: the
# licence someone read in that file, and the file's sha256, so a change to it
# fails the check until someone reads it again.
LICENSE_FILE_ONLY = {
    "allo-isolate": ("Apache-2.0", "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4"),
}

# Licence and notice files, wherever they are in a crate: vendored C code
# (aws-lc in aws-lc-sys, the Dart SDK headers in dart-sys) brings its own.
# Everything in a REUSE-style LICENSES folder counts too.
LICENSE_NAME = re.compile(r"(?:LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICENSE)S?(?:[-_.].*)?", re.I)
LICENSE_DIR = re.compile(r"LICEN[CS]ES?", re.I)
# Source files that a licence-like name can still match (src/license.rs).
CODE = frozenset({
    ".asm", ".c", ".cc", ".cpp", ".go", ".h", ".hpp", ".in", ".js", ".json", ".pl",
    ".py", ".rb", ".rs", ".s", ".sh", ".toml", ".ts", ".yaml", ".yml",
})

RULE = "=" * 78
THIN_RULE = "-" * 78


@dataclass(frozen=True)
class Crate:
    name: str
    version: str
    # Cargo.toml's `license`, an SPDX expression; None if it only names a file.
    license: str | None
    license_file: str | None
    # Where cargo unpacked it.
    path: Path
    repository: str | None = None

    def __str__(self) -> str:
        return f"{self.name} {self.version}"


@dataclass(frozen=True)
class Text:
    # Where the text came from, for the reader: the file in the crate, or the
    # one checked in here.
    source: str
    body: str


def accepted(expression: str) -> bool:
    """Whether a `license` expression can be met with ACCEPTED licences: one
    side of every OR, both sides of every AND. Old crates write OR as `/`.
    Raises ValueError if it isn't an expression."""
    tokens = re.findall(r"[()/]|[^\s()/]+", expression)
    pos = 0

    def peek() -> str | None:
        return tokens[pos].upper() if pos < len(tokens) else None

    def take() -> str:
        nonlocal pos
        if pos == len(tokens):
            raise ValueError(f"{expression!r} ends too soon")
        pos += 1
        return tokens[pos - 1]

    def either() -> bool:
        ok = both()
        while peek() in ("OR", "/"):
            take()
            ok = both() or ok
        return ok

    def both() -> bool:
        ok = one()
        while peek() == "AND":
            take()
            ok = one() and ok
        return ok

    def one() -> bool:
        token = take()
        if token == "(":
            ok = either()
            if take() != ")":
                raise ValueError(f"{expression!r} has an unclosed (")
            return ok
        if token.upper() in ("AND", "OR", "WITH") or token in ("/", ")"):
            raise ValueError(f"{expression!r} has {token!r} where a licence should be")
        if peek() == "WITH":
            # An exception only adds permissions.
            take()
            take()
        # GPL-2.0+ is that version or a later one: the one named will do.
        return token.removesuffix("+") in ACCEPTED

    ok = either()
    if pos != len(tokens):
        raise ValueError(f"{expression!r} has {tokens[pos]!r} where AND or OR should be")
    return ok


def license_files(crate: Path, license_file: str | None = None) -> list[Path]:
    """The licence and notice files in a crate, the top-level ones first."""
    found = set()
    for dirpath, dirnames, filenames in os.walk(crate):
        dirnames.sort()
        here = Path(dirpath)
        in_license_dir = any(LICENSE_DIR.fullmatch(part) for part in here.relative_to(crate).parts)
        for name in filenames:
            if Path(name).suffix.lower() in CODE:
                continue
            if in_license_dir or LICENSE_NAME.fullmatch(name):
                found.add(here / name)
    if license_file and (crate / license_file).is_file():
        path = Path(os.path.normpath(crate / license_file))
        if path.is_relative_to(crate):
            found.add(path)
    return sorted(found, key=lambda p: (len(p.relative_to(crate).parts), p.relative_to(crate).as_posix()))


def own_license(crate: Crate, path: Path) -> bool:
    """Whether a file license_files found is the crate's own licence: at its
    top level, in a top-level LICENSES folder, or the `license-file`."""
    parts = path.relative_to(crate.path).parts
    return len(parts) == 1 or bool(LICENSE_DIR.fullmatch(parts[0])) or path == crate.path / (crate.license_file or "")


def normalize(data: bytes) -> str:
    """A licence text as printed: Unix line ends, no trailing spaces or blank
    lines around it. So copies that differ only in those count as one."""
    try:
        text = data.decode("utf-8-sig")
    except UnicodeDecodeError:
        text = data.decode("latin-1")
    lines = [line.rstrip() for line in text.replace("\r\n", "\n").replace("\r", "\n").split("\n")]
    while lines and not lines[0]:
        lines.pop(0)
    while lines and not lines[-1]:
        lines.pop()
    return "\n".join(lines) + "\n"


def parse_tree(output: str) -> set[tuple[str, str]]:
    """The (name, version) of each line `cargo tree --prefix none --format {p}`
    prints, like `serde_derive v1.0.228 (proc-macro)`."""
    crates = set()
    for line in output.splitlines():
        if not line.strip():
            continue
        m = re.match(r"(\S+) v(\S+)", line)
        if not m:
            raise SystemExit(f"cargo tree: expected `name vVERSION`, not {line!r}")
        crates.add((m.group(1), m.group(2)))
    return crates


def cargo(root: Path, *args: str) -> str:
    command = ["cargo", *args, "--manifest-path", str(root / WORKSPACE), "--locked"]
    try:
        result = subprocess.run(command, capture_output=True, text=True, check=False)
    except FileNotFoundError:
        raise SystemExit("rust_notices.py needs cargo") from None
    if result.returncode:
        raise SystemExit(f"{' '.join(command)} failed:\n{result.stderr}")
    return result.stdout


def version_key(version: str) -> tuple:
    return tuple(int(n) for n in re.findall(r"\d+", version.split("+")[0])[:3]), version


def shipped(root: Path) -> list[Crate]:
    """The crates in the Flatpak's build of the Rust core, not ReGa's own."""
    tree = cargo(root, "tree", "-p", PACKAGE, "--target", TARGET, "-e", "normal,build",
                 "--prefix", "none", "--format", "{p}")
    wanted = parse_tree(tree)
    metadata = json.loads(cargo(root, "metadata", "--format-version", "1", "--filter-platform", TARGET))
    packages = {}
    for package in metadata["packages"]:
        key = (package["name"], package["version"])
        if key in wanted and key in packages:
            # cargo tree names a crate by name and version only.
            raise SystemExit(f"two crates are {key[0]} {key[1]} (from crates.io and git, say): "
                             f"rust_notices.py can't tell which one is in the build")
        packages[key] = package
    if wanted - set(packages):
        raise SystemExit(f"cargo metadata has nothing for {sorted(wanted - set(packages))}, which cargo tree lists")
    ours = set(metadata["workspace_members"])
    crates = [
        Crate(
            name=package["name"],
            version=package["version"],
            license=package.get("license"),
            license_file=package.get("license_file"),
            path=Path(package["manifest_path"]).parent,
            repository=package.get("repository"),
        )
        for package in (packages[key] for key in wanted)
        if package["id"] not in ours
    ]
    return sorted(crates, key=lambda c: (c.name, version_key(c.version)))


@dataclass(frozen=True)
class Entry:
    crate: Crate
    # The crate's licence: its `license`, or what LICENSE_FILE_ONLY says.
    license: str
    texts: tuple[Text, ...]


def collect(root: Path, crates: list[Crate]) -> tuple[list[Entry], list[str], list[str]]:
    """Each crate with its licence texts; then the problems that leave the
    notices incomplete, and notes on entries here that aren't needed any
    more."""
    where = "app/flatpak/rust_notices.py"
    names = {crate.name for crate in crates}
    entries = []
    errors = []
    notes = [f"{table} has {name}, which isn't in the build any more"
             for table, names_there in (("UNSHIPPED", UNSHIPPED), ("LICENSE_FILE_ONLY", LICENSE_FILE_ONLY))
             for name in sorted(set(names_there) - names)]
    for crate in crates:
        at = f"{crate} ({crate.repository})" if crate.repository else str(crate)

        license = crate.license
        if license is None and crate.name in LICENSE_FILE_ONLY:
            license, sha256 = LICENSE_FILE_ONLY[crate.name]
            path = crate.path / (crate.license_file or "")
            actual = hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None
            if actual != sha256:
                errors.append(
                    f"{at}: its licence file {crate.license_file} isn't the one read as {license}. Read it, "
                    f"then set its licence and sha256 ({actual}) in LICENSE_FILE_ONLY in {where}.")
        elif license is None:
            errors.append(
                f"{at} has no `license` in its Cargo.toml, only the file {crate.license_file}. Read it, and "
                f"add the crate to LICENSE_FILE_ONLY in {where}.")
        elif crate.name in LICENSE_FILE_ONLY:
            notes.append(f"LICENSE_FILE_ONLY has {crate.name}, whose Cargo.toml says `{license}` now")
        if license is not None:
            try:
                ok = accepted(license)
            except ValueError as e:
                ok = False
                errors.append(f"{at}: can't read its licence: {e}")
            else:
                if not ok:
                    errors.append(
                        f"{at} is under {license}, which shipping its notice doesn't cover. Read its terms: "
                        f"if that's all they ask, add the licence to ACCEPTED in {where}.")

        files = license_files(crate.path, crate.license_file)
        texts = [Text(path.relative_to(crate.path).as_posix(), normalize(path.read_bytes())) for path in files]
        # Its own licence is at its top level; deeper ones are vendored code's.
        own = any(own_license(crate, path) for path in files)
        if own and crate.name in UNSHIPPED:
            notes.append(f"{crate} ships its licence text now: drop it from UNSHIPPED and {TEXTS}/")
        elif crate.name in UNSHIPPED:
            checked_in = [Text(f"{TEXTS}/{crate.name}/{path.name}, from its repository", normalize(path.read_bytes()))
                          for path in sorted((root / TEXTS / crate.name).glob("*")) if path.is_file()]
            if not checked_in:
                errors.append(f"{crate}: nothing in {TEXTS}/{crate.name}/")
            if crate.license != UNSHIPPED[crate.name]:
                errors.append(
                    f"{at}: its Cargo.toml says `{crate.license}` now, but the text in {TEXTS}/{crate.name}/ "
                    f"was taken for `{UNSHIPPED[crate.name]}`. Take it again and update UNSHIPPED in {where}.")
            texts = checked_in + texts
        elif not own:
            errors.append(
                f"{at} ships no licence text of its own. Copy it from the crate's repository, at the commit in "
                f"the crate's .cargo_vcs_info.json, into {TEXTS}/{crate.name}/, and add the crate to UNSHIPPED "
                f"in {where}.")
        entries.append(Entry(crate, license or "unknown", tuple(texts)))
    return entries, errors, notes


def render(lock_sha256: str, entries: list[Entry]) -> str:
    out = [
        "Licences of the Rust crates in ReGa",
        RULE,
        "",
        "ReGa's Rust core (MIT, like the rest of ReGa: see LICENSE) is built with",
        "the crates below, at these versions. Their licence texts follow, as each",
        "crate ships them. A text that several crates ship word for word is",
        "printed once, under all of their names.",
        "",
        f"Made by app/flatpak/rust_notices.py from app/Cargo.lock, sha256 {lock_sha256},",
        f"for {TARGET}.",
        "",
        "",
        "Crates",
        THIN_RULE,
        "",
    ]
    width = max((len(str(entry.crate)) for entry in entries), default=0)
    out += [f"{str(entry.crate):<{width}}  {entry.license}" for entry in entries]
    by_body: dict[str, list[str]] = {}
    for entry in entries:
        for text in entry.texts:
            by_body.setdefault(text.body, []).append(f"{entry.crate}: {text.source}")
    out += ["", "", "Licence texts", THIN_RULE]
    for body, sources in by_body.items():
        out += ["", RULE, *sources, THIN_RULE, "", body.rstrip("\n")]
    return "\n".join(out) + "\n"


def notices(root: Path) -> tuple[str, int, list[str], list[str]]:
    """The notices file, how many crates it covers, and what collect found."""
    entries, errors, notes = collect(root, shipped(root))
    lock_sha256 = hashlib.sha256((root / LOCK).read_bytes()).hexdigest()
    return render(lock_sha256, entries), len(entries), errors, notes


def main(argv: list[str]) -> int:
    if argv == ["check"]:
        out = None
    elif len(argv) == 2 and argv[0] == "write":
        out = Path(argv[1])
    else:
        print(__doc__, file=sys.stderr)
        return 2
    text, count, errors, notes = notices(ROOT)
    for note in notes:
        print(f"note: {note}", file=sys.stderr)
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    if errors:
        return 1
    if out:
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(text, encoding="utf-8")
        print(f"Wrote the licence texts of {count} Rust crates to {out}.")
    else:
        print(f"Found licence texts for all {count} Rust crates.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
