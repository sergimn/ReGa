#!/usr/bin/env python3
"""Pins for the tools the Flatpak bundles next to the app.

    python3 app/flatpak/pins.py update   # regenerate ansible.yml (needs network)
    python3 app/flatpak/pins.py check    # offline; CI runs it

The Flatpak bundles Ansible at exactly the versions CI tests: the packages
and hashes in infra/ansible/requirements.lock, and the collections in
requirements.yml. Renovate keeps both, and only takes releases at least 30
days old (see CONTRIBUTING.md), so nothing here picks versions. `update`
takes, for each package in the lock, the wheel among the lock's hashes that
pip would install on the runtime, and writes the manifest module ansible.yml
with each file's URL and sha256. `check` fails when ansible.yml, the lock,
requirements.txt and the Terraform CI tests with no longer agree.
Only stdlib, so it runs on a bare CI runner.
"""

from __future__ import annotations

import json
import re
import sys
import urllib.request
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = "app/flatpak/io.github.sergimn.rega.yml"
ANSIBLE_MODULE = "app/flatpak/ansible.yml"
REQUIREMENTS = "infra/ansible/requirements.txt"
LOCK = "infra/ansible/requirements.lock"
COLLECTIONS = "infra/ansible/requirements.yml"
TERRAFORM_WORKFLOW = ".github/workflows/terraform.yml"


@dataclass(frozen=True)
class Runtime:
    python: str
    glibc: tuple[int, int]


# What each runtime ships. Compiled wheels only load on its Python and glibc,
# so a runtime bump needs a new entry here and `update`.
RUNTIMES = {"25.08": Runtime(python="3.13", glibc=(2, 42))}

# The manifest only builds for x86_64 (Terraform and the Flutter bundle too).
ARCH = "x86_64"

GALAXY = "https://galaxy.ansible.com/api/v3/plugin/ansible/content/published/collections/index"


@dataclass(frozen=True)
class Source:
    filename: str
    url: str
    sha256: str


@dataclass(frozen=True)
class Locked:
    name: str
    version: str
    marker: str
    hashes: frozenset[str]


@dataclass(frozen=True)
class Pin:
    name: str
    extras: str
    version: str

    def __str__(self) -> str:
        return f"{self.name}{self.extras}=={self.version}"


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


def python_pins(root: Path) -> dict[str, Pin]:
    """Each `name[extras]==version` line of requirements.txt, by name."""
    pins = {}
    for line in read(root, REQUIREMENTS).splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        m = re.fullmatch(r"([A-Za-z0-9][A-Za-z0-9._-]*)\s*(\[[^\]]*\])?\s*==\s*([^\s;*=]+)", line)
        if not m:
            raise SystemExit(f"{REQUIREMENTS}: pin every package exactly, not {line!r}")
        extras = sorted(normalize(e) for e in (m.group(2) or "[]")[1:-1].split(",") if e.strip())
        name = normalize(m.group(1))
        pins[name] = Pin(name, f"[{','.join(extras)}]" if extras else "", m.group(3))
    if not pins:
        raise SystemExit(f"{REQUIREMENTS}: no packages")
    return pins


def locked_packages(root: Path) -> dict[str, Locked]:
    """Every package in requirements.lock, by name, as uv writes them:
    `name==version [; marker] --hash=sha256:...`, one or more hashes."""
    packages = {}
    for line in read(root, LOCK).replace("\\\n", " ").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        m = re.fullmatch(
            r"([A-Za-z0-9][A-Za-z0-9._-]*)(?:\[[^\]]*\])?==(\S+?)\s*(?:;\s*(.*?))?\s*"
            r"((?:--hash=sha256:[0-9a-f]{64}\s*)+)",
            line,
        )
        if not m:
            raise SystemExit(f"{LOCK}: expected `name==version --hash=sha256:...`, not {line[:80]!r}")
        name = normalize(m.group(1))
        hashes = frozenset(re.findall(r"sha256:([0-9a-f]{64})", m.group(4)))
        packages[name] = Locked(name, m.group(2), m.group(3) or "", hashes)
    if not packages:
        raise SystemExit(f"{LOCK}: no packages")
    return packages


def marker_environment(runtime: Runtime) -> dict[str, str]:
    """PEP 508 marker values inside the Flatpak."""
    return {
        "implementation_name": "cpython",
        "os_name": "posix",
        "platform_machine": ARCH,
        "platform_python_implementation": "CPython",
        "platform_system": "Linux",
        "python_version": runtime.python,
        "sys_platform": "linux",
    }


MARKER_TOKEN = re.compile(
    r"\s*([a-z_]+\s*(?:==|!=|<=|>=|<|>)\s*(?:'[^']*'|\"[^\"]*\")|\(|\)|and\b|or\b)"
)


def marker_applies(marker: str, env: dict[str, str]) -> bool:
    """Whether a lock line's marker holds in env. Reads the comparisons
    joined by `and`, `or` and parentheses that uv writes, and refuses
    anything else rather than guess."""
    marker = marker.strip()
    if not marker:
        return True
    tokens, pos = [], 0
    while pos < len(marker):
        m = MARKER_TOKEN.match(marker, pos)
        if not m:
            raise SystemExit(f"{LOCK}: can't read the marker {marker!r}")
        tokens.append(m.group(1))
        pos = m.end()

    def compare(token: str) -> bool:
        var, op, value = re.fullmatch(r"([a-z_]+)\s*(\S+?)\s*['\"](.*)['\"]", token).groups()
        if var not in env:
            raise SystemExit(f"{LOCK}: pins.py doesn't know the marker {var}; add it to marker_environment().")
        if op in ("==", "!="):
            return (env[var] == value) == (op == "==")
        if var != "python_version":
            raise SystemExit(f"{LOCK}: can't compare {var} with {op}")
        have, want = (tuple(int(x) for x in v.split(".")) for v in (env[var], value))
        return {"<": have < want, "<=": have <= want, ">": have > want, ">=": have >= want}[op]

    def expression() -> bool:
        value = term()
        while tokens and tokens[0] == "or":
            tokens.pop(0)
            right = term()
            value = value or right
        return value

    def term() -> bool:
        value = atom()
        while tokens and tokens[0] == "and":
            tokens.pop(0)
            right = atom()
            value = value and right
        return value

    def atom() -> bool:
        token = tokens.pop(0) if tokens else ")"
        if token == "(":
            value = expression()
            if not tokens or tokens.pop(0) != ")":
                raise SystemExit(f"{LOCK}: unbalanced marker {marker!r}")
            return value
        if token in (")", "and", "or"):
            raise SystemExit(f"{LOCK}: can't read the marker {marker!r}")
        return compare(token)

    result = expression()
    if tokens:
        raise SystemExit(f"{LOCK}: can't read the marker {marker!r}")
    return result


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


def same_version(a: str, b: str) -> bool:
    """1.0 and 1.0.0 are the same release; a wheel's name can have either."""
    return re.sub(r"(\.0)+$", "", a) == re.sub(r"(\.0)+$", "", b)


def wheel_tags(filename: str) -> tuple[str, str, list[tuple[str, str, str]]]:
    """(normalized name, version, every python-abi-platform tag) of a wheel."""
    parts = filename.removesuffix(".whl").split("-")
    # name-version[-build]-python-abi-platform; each tag can be a.b compressed.
    if not filename.endswith(".whl") or len(parts) not in (5, 6):
        raise SystemExit(f"{filename} isn't a wheel")
    py, abi, platform = parts[-3:]
    tags = [(p, a, pl) for p in py.split(".") for a in abi.split(".") for pl in platform.split(".")]
    return normalize(parts[0]), parts[1], tags


LEGACY_MANYLINUX = {"manylinux1": (2, 5), "manylinux2010": (2, 12), "manylinux2014": (2, 17)}


def manylinux_glibc(platform: str) -> tuple[int, int] | None:
    """The glibc a manylinux tag for ARCH needs; None for any other platform."""
    if m := re.fullmatch(rf"manylinux_(\d+)_(\d+)_{re.escape(ARCH)}", platform):
        return int(m.group(1)), int(m.group(2))
    if m := re.fullmatch(rf"(manylinux\d+)_{re.escape(ARCH)}", platform):
        return LEGACY_MANYLINUX.get(m.group(1))
    return None


def interpreter_rank(py: str, abi: str, minor: int) -> int | None:
    """pip's order for a wheel's python and ABI tags on CPython 3.<minor>:
    its own ABI, then abi3, then pure Python; None if it can't load."""
    cp = f"cp3{minor}"
    if py == cp:
        return {cp: 0, "abi3": 1, "none": 2}.get(abi)
    if (m := re.fullmatch(r"cp3(\d+)", py)) and abi == "abi3" and int(m.group(1)) < minor:
        return 2 + minor - int(m.group(1))
    if (m := re.fullmatch(r"py3(\d*)", py)) and abi == "none":
        if not m.group(1):
            return 101
        if int(m.group(1)) <= minor:
            return 100 if int(m.group(1)) == minor else 102 + minor - int(m.group(1))
    return None


def wheel_rank(filename: str, runtime: Runtime) -> tuple | None:
    """Where pip on the runtime ranks this wheel (lower is better), or None
    if it can't install it: manylinux before `any`, then by interpreter
    (see interpreter_rank), then the newest glibc first."""
    minor = int(runtime.python.split(".")[1])
    ranks = []
    for py, abi, platform in wheel_tags(filename)[2]:
        if platform == "any":
            platform_rank = (1, 0, 0)
        elif (glibc := manylinux_glibc(platform)) and glibc <= runtime.glibc:
            platform_rank = (0, -glibc[0], -glibc[1])
        else:
            continue
        if (interpreter := interpreter_rank(py, abi, minor)) is not None:
            ranks.append((platform_rank[0], interpreter, platform_rank[1:]))
    return min(ranks, default=None)


def best_wheel(filenames, runtime: Runtime) -> str | None:
    """The wheel pip would install on the runtime, of one package's files."""
    ranked = [(rank, f) for f in filenames if (rank := wheel_rank(f, runtime)) is not None]
    return min(ranked)[1] if ranked else None


def module_sources(text: str) -> list[Source]:
    """The sources of a generated ansible.yml."""
    pairs = re.findall(r"^\s*url:\s*\"?([^\s\"]+)\"?\s*\n\s*sha256:\s*([0-9a-f]{64})\s*$", text, re.M)
    if len(pairs) != len(re.findall(r"^\s*url:", text, re.M)):
        raise SystemExit(f"{ANSIBLE_MODULE}: give every source a url and then its sha256")
    return [Source(url.rsplit("/", 1)[1], url, digest) for url, digest in pairs]


def check(root: Path) -> list[str]:
    """Every way the manifest has drifted from the repo's pins."""
    errors = []

    bundled, ci = terraform_versions(root)
    if bundled != ci:
        errors.append(f"The Flatpak bundles Terraform {bundled} but CI tests the module with {ci}.")

    lock = locked_packages(root)
    for pin in python_pins(root).values():
        locked = lock.get(pin.name)
        if locked is None or not same_version(locked.version, pin.version):
            have = f"{locked.name}=={locked.version}" if locked else "nothing"
            errors.append(f"{REQUIREMENTS} pins {pin}, but {LOCK} has {have}; "
                          "regenerate the lock with the command in its header.")

    runtime_name = runtime_version(root)
    runtime = RUNTIMES.get(runtime_name)
    if runtime is None:
        errors.append(f"Runtime {runtime_name} isn't in RUNTIMES in pins.py; add its Python and glibc.")

    module = root / ANSIBLE_MODULE
    if not module.exists():
        return errors + [f"{ANSIBLE_MODULE} is missing; run pins.py update."]
    text = module.read_text(encoding="utf-8")
    if not re.search(rf"^\s*-\s*\"?{re.escape(Path(ANSIBLE_MODULE).name)}\"?\s*$", read(root, MANIFEST), re.M):
        errors.append(f"{MANIFEST} doesn't include {Path(ANSIBLE_MODULE).name} in its modules.")
    sources = module_sources(text)

    if runtime:
        if f"/lib/python{runtime.python}/" not in text:
            errors.append(f"{ANSIBLE_MODULE} installs for another Python than the runtime's {runtime.python}.")
        wheels: dict[str, tuple[str, Source]] = {}
        for source in (s for s in sources if s.filename.endswith(".whl")):
            name, version, _ = wheel_tags(source.filename)
            if name in wheels:
                errors.append(f"{ANSIBLE_MODULE} has two wheels of {name}.")
            wheels[name] = (version, source)
            if wheel_rank(source.filename, runtime) is None:
                errors.append(f"{ANSIBLE_MODULE} has {source.filename}, which doesn't install on the "
                              f"{runtime_name} runtime (Python {runtime.python}, {ARCH}).")
        env = marker_environment(runtime)
        wanted = {name: p for name, p in lock.items() if marker_applies(p.marker, env)}
        for name, locked in sorted(wanted.items()):
            if name not in wheels:
                errors.append(f"{LOCK} has {name}=={locked.version}; {ANSIBLE_MODULE} has nothing.")
                continue
            version, source = wheels[name]
            if not same_version(version, locked.version):
                errors.append(f"{LOCK} has {name}=={locked.version}; {ANSIBLE_MODULE} has {version}.")
            elif source.sha256 not in locked.hashes:
                errors.append(f"{ANSIBLE_MODULE} has {source.filename}, whose sha256 isn't one of {LOCK}'s.")
        for name in sorted(wheels.keys() - wanted.keys()):
            errors.append(f"{ANSIBLE_MODULE} has {wheels[name][1].filename}, which {LOCK} doesn't have.")

    tarballs = {s.filename for s in sources if s.filename.endswith(".tar.gz")}
    wanted_tarballs = {f"{n.replace('.', '-')}-{v}.tar.gz" for n, v in collection_pins(root).items()}
    for missing in sorted(wanted_tarballs - tarballs):
        errors.append(f"{COLLECTIONS} wants {missing}, which {ANSIBLE_MODULE} doesn't have.")
    for extra in sorted(tarballs - wanted_tarballs):
        errors.append(f"{ANSIBLE_MODULE} has {extra}, which {COLLECTIONS} doesn't list.")

    if errors:
        errors.append(f"Run `python3 app/flatpak/pins.py update` after {LOCK} or {COLLECTIONS} changes.")
    return errors


def fetch_json(url: str) -> dict:
    with urllib.request.urlopen(url, timeout=60) as response:
        return json.load(response)


def resolve_wheels(root: Path, runtime: Runtime) -> list[Source]:
    """For each package the lock has for the runtime, the wheel pip would
    install there, from the files on PyPI whose sha256 the lock lists."""
    env = marker_environment(runtime)
    sources = []
    for name, locked in sorted(locked_packages(root).items()):
        if not marker_applies(locked.marker, env):
            continue
        release = fetch_json(f"https://pypi.org/pypi/{name}/{locked.version}/json")
        files = {u["filename"]: u for u in release["urls"]
                 if u["packagetype"] == "bdist_wheel" and u["digests"]["sha256"] in locked.hashes}
        best = best_wheel(files, runtime)
        if best is None:
            raise SystemExit(f"None of the {name} {locked.version} wheels in {LOCK} installs on "
                             f"Python {runtime.python} for {ARCH}, and the Flatpak doesn't build from source.")
        sources.append(Source(best, files[best]["url"], files[best]["digests"]["sha256"]))
    return sources


def resolve_collections(root: Path) -> list[Source]:
    sources = []
    for name, version in collection_pins(root).items():
        namespace, collection = name.split(".", 1)
        info = fetch_json(f"{GALAXY}/{namespace}/{collection}/versions/{version}/")
        artifact = info["artifact"]
        sources.append(Source(artifact["filename"], info["download_url"], artifact["sha256"]))
    return sources


def render(runtime: Runtime, wheels: list[Source], collections: list[Source]) -> str:
    q = json.dumps  # a JSON string is a valid YAML double-quoted scalar
    site_packages = f"${{FLATPAK_DEST}}/lib/python{runtime.python}/site-packages"
    tarballs = " ".join(s.filename for s in collections)
    lines = [
        f"# Generated by `python3 app/flatpak/pins.py update` from {LOCK}",
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
        "  - " + q(f"/lib/python{runtime.python}/site-packages/ansible_test"),
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
    runtime_name = runtime_version(root)
    runtime = RUNTIMES.get(runtime_name)
    if runtime is None:
        raise SystemExit(f"Runtime {runtime_name} isn't in RUNTIMES; add its Python and glibc first.")
    text = render(runtime, resolve_wheels(root, runtime), resolve_collections(root))
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
