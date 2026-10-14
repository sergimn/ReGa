# Flatpak

ReGa ships as the Flatpak `io.github.sergimn.rega`, for x86_64, on the
`org.freedesktop.Platform` 25.08 runtime from Flathub.

## Install

Each [release](https://github.com/sergimn/ReGa/releases) has the bundle as
`rega-x86_64.flatpak`. On Ubuntu:

```sh
sudo apt install flatpak
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
wget -O rega-x86_64.flatpak https://github.com/sergimn/ReGa/releases/latest/download/rega-x86_64.flatpak
flatpak install --user rega-x86_64.flatpak     # also pulls the runtime from Flathub
flatpak install --user flathub com.moonlight_stream.Moonlight   # to play
flatpak run io.github.sergimn.rega
```

To update, install a newer bundle the same way. Uninstalling
(`flatpak uninstall --user io.github.sergimn.rega`) doesn't touch your AWS
account, so destroy the machines you no longer want in the app first.

For a build of something unreleased, every CI run of the App workflow also
builds `rega.flatpak`: download the `rega-flatpak` artifact from the run's
page, `unzip rega-flatpak.zip`, and install `rega.flatpak` as above.

## What's inside

| Path in the sandbox | What |
| --- | --- |
| `/app/lib/rega`, `/app/bin/rega` | The app: the Flutter release bundle with the Rust core, built in the Flatpak build |
| `/app/bin/terraform` | Terraform, the version `.github/workflows/terraform.yml` tests with |
| `/app/bin/ansible-playbook` and friends | Ansible with pywinrm, at the versions in `infra/ansible/requirements.lock`; the app runs the playbook it carries with it |
| `/app/lib/python3.13/site-packages/ansible_collections` | The collections in `infra/ansible/requirements.yml`, found without any config |
| `/app/share/licenses/io.github.sergimn.rega` | ReGa's licence (MIT), Terraform's (BUSL-1.1) in `terraform/`, the licence notices of the Rust crates in the app and of Rust's standard library, in `rust-crates.txt` and `rust-std.html`, and where the AWS logo in the app comes from, in `aws-logo.md`. Flutter keeps those of the Dart packages and its engine in `/app/lib/rega/data/flutter_assets/NOTICES.Z` |

Permissions: network, display (Wayland, or X11 as a fallback) and GPU.
Nothing in your home folder, `~/.aws` included: ReGa only uses the cloud
credentials entered in the app. It keeps them in the system keyring through
the Secret portal, which needs no permission: the portal hands the app a key,
and the app keeps its credentials in a file encrypted with it under
`~/.var/app/io.github.sergimn.rega/data/keyrings/`. GNOME Keyring provides
the portal on Ubuntu.

Things to know when the app runs these tools:

- Terraform and Ansible run inside the sandbox. Quitting the app mid-change
  doesn't stop Terraform: the sandbox stays up, without a window, until
  Terraform finishes and releases its state lock.
- Each launch of the app gets a new sandbox, which can't see processes in an
  earlier one. So a reopened app can't tell whether a run it finds there is
  still going, and goes by the state lock instead: the machine shows busy
  while the lock is held. A lock left by a run that was killed (say, by
  logging out) counts as stale after two hours, or at the first change
  after a reboot if nothing else ran on that machine before it.
- The AWS Terraform provider isn't bundled; Terraform downloads it on first
  use into the app's cache.
- Inside the sandbox, `$HOME` is a scratch folder that is emptied when the
  sandbox exits. That suits `~/.ansible` (Ansible's temp files) and
  `~/.terraform.d`. Anything that must survive goes under `$XDG_DATA_HOME`
  or `$XDG_CACHE_HOME`, which point into `~/.var/app/io.github.sergimn.rega/`;
  the playbook already keeps its WireGuard keys and Sunshine password there.

## Build

flatpak-builder builds the app from source, as Flathub would: with the
Flutter SDK and Rust toolchain CI uses, and the crates and Dart packages in
the app's locks. It downloads all of them first, checking each against its
SHA-256 (Flutter's git repository, against its commit), and then builds
with no network (see Sources below). From `app/`:

```sh
python3 flatpak/sources.py write
flatpak-builder --user --install --install-deps-from=flathub --force-clean \
  build/flatpak flatpak/io.github.sergimn.rega.yml
dbus-run-session -- flatpak/smoke-test.sh
```

`sources.py write` puts the sources of the crates and Dart packages in the
locks in `flatpak/generated/`, which the manifest reads; run it again after
a lock changes. The first build takes a while, since it builds Flutter's
tool and then the whole Rust core; flatpak-builder caches each module, so a
rebuild after a change to the app only redoes the app.

The smoke test checks the installed app: the licences are there (the Rust
crates' for the `Cargo.lock` you built from), Terraform and Ansible run in
the sandbox, the Windows collections are there, the playbooks in
`infra/ansible` pass a syntax check, and the app draws its window (on demo
data, on a private Xvfb display over X11, whatever your desktop runs). It
needs Xvfb and xdotool.

CI also runs Flathub's linter, `flatpak-builder-lint`, on the manifest and
the build. `lint-exceptions.json` leaves out the one check only Flathub's own
build can pass: Flathub copies the screenshots to its server as it builds.
The build still fetches the screenshot, from `docs/screenshot.png` on main,
to describe the app to software centres; if it can't, the linter fails the
build for having no screenshot.

## Sources

Every source of the build is pinned by hash, and none of them picks a
version: they follow what CI builds with and the locks.

| Source | What | Where it comes from |
| --- | --- | --- |
| `flutter.yml` | The Flutter SDK at `FLUTTER_VERSION`'s commit, what its tool downloads for a Linux release build (the Dart SDK, the engine, fonts), and the Dart packages the tool is built from | `sources.py update` |
| `rust.yml` | Rust at `RUST_VERSION` (`rustc`, `cargo`, the standard library) | `sources.py update`, with the hashes in the release's channel manifest |
| `generated/cargo-sources.json` | Every crate in `app/Cargo.lock`, vendored | `sources.py write`, with the lock's checksums |
| `generated/pub-sources.json` | Every Dart package in `app/pubspec.lock` and in cargokit's `rust_builder/cargokit/build_tool/pubspec.lock` | `sources.py write`, with the locks' hashes. cargokit's build tool runs without its lock, so it may get the app's newer version of a package both have |

`FLUTTER_VERSION` and `RUST_VERSION` are in `.github/workflows/app.yml`.
After either changes, regenerate the two modules (this needs network and
git, nothing else) and commit them:

```sh
python3 flatpak/sources.py update
```

Renovate's Flutter and Rust PRs say so, and `python3 flatpak/sources.py
check` (CI runs it) fails until then. It also fails when a lock has
something `write` can't make an offline source of, such as a git
dependency, or when `flutter.yml` downloads another engine, fonts or Gradle
wrapper than the ones its build checks Flutter's commit asks for. The
generated files aren't committed, so Renovate's crate and Dart package
updates need nothing by hand.

The SDK and toolchain go in `/var/lib`, which the build of the app uses and
the Flatpak doesn't ship. Three small things make the build work offline:

- `patches/flutter-offline.patch`: the Flutter tool builds itself with
  `dart pub upgrade`, which needs the network; this adds `--offline`. If a
  new Flutter changes that line, `sources.py update` stops and says so.
- `patches/cargokit-offline.patch`: the same for cargokit's build tool,
  which builds the Rust core during `flutter build`.
- `rustup.sh`: cargokit runs cargo through rustup, and the build has one
  toolchain and no rustup, so this answers cargokit's few rustup calls.

Flutter builds the app's runner with clang, from the
`org.freedesktop.Sdk.Extension.llvm21` SDK extension, which comes from
Flathub at its current 25.08 build, as the runtime does.

## Ansible pins

`ansible.yml` is generated: a wheel for every package in
`infra/ansible/requirements.lock`, at the lock's version and with one of its
SHA-256 hashes, and the Galaxy collections in `requirements.yml`, each with
its URL and sha256. So the Flatpak runs exactly the Ansible that CI tests,
held to the same 30-day rule (see `CONTRIBUTING.md`). Of the wheels the lock
allows, it takes the one pip would install on the runtime's Python.

After the lock or `requirements.yml` changes, or the runtime moves to a
different Python, regenerate it (this needs network, nothing else):

```sh
python3 flatpak/pins.py update
```

Renovate's PRs for these files say so. `python3 flatpak/pins.py check` runs
in CI and fails when the manifest no longer matches the lock, the lock no
longer matches `requirements.txt`, or the bundled Terraform isn't the one
CI tests the module with.

## Rust crate licences

The crates in the app's Rust core and Rust's standard library (MIT,
Apache-2.0, BSD and the like) ask for their notices and licence texts to go
with every copy. `rust_notices.py write DIR` puts two files in `DIR`:

- `rust-crates.txt`: the crates and versions `cargo tree` finds for the
  build (normal and build dependencies of `rega_bridge` on x86_64 Linux),
  then the licence texts each crate ships, vendored code's too (aws-lc in
  `aws-lc-sys`, say), each text once under all the crates that ship it.
- `rust-std.html`: the standard library's notices, copied from the Rust
  toolchain that builds the core (`share/doc/rust/COPYRIGHT-library.html`
  in its sysroot).

The Flatpak build writes them, from the crates it builds with, so they
always match `Cargo.lock` and Renovate's crate updates need nothing by hand.
The smoke test checks the installed `rust-crates.txt` was written for the
`Cargo.lock` of your checkout.

`python3 flatpak/rust_notices.py check` (the App workflow's Rust job runs it)
fails, saying what to do, when:

- A crate is under a licence that isn't in `ACCEPTED`, the ones whose terms
  a binary meets by shipping the notice. Read its terms before adding it.
- A crate ships no licence text of its own. Copy its text from its
  repository, at the commit the error names, into `rust-licenses/<crate>/`,
  and add the crate to `UNSHIPPED` with its licence and that commit.
- A crate's Cargo.toml has only a `license-file`, no licence: read the file
  and add the crate to `LICENSE_FILE_ONLY` with the file's sha256.
- One of those crates changes its licence, or that file.

Texts checked in here can go stale, so for them the check warns rather
than failing (on GitHub, as an annotation on the run and the PR): when an
`UNSHIPPED` crate is published from a new commit, or an `aws-lc-sys` update
arrives for `LEFT_OUT` (the licences of vendored code that aws-lc's own
licence points at but the crate leaves out), see whether the text changed
and update the entry.

It doesn't check that a crate's files cover every licence in an AND: a crate
under `MIT AND Unicode-3.0` that ships only the MIT text would pass. The
crates in the build today ship all of theirs.

## Not done yet

- Launching Moonlight from inside the sandbox. The Moonlight Flatpak is a
  separate app, so starting it needs either a host permission or bundling
  Moonlight here. Until then, Play pairs by hand: ReGa shows the host to add
  in Moonlight and asks for the PIN Moonlight shows (see the app README's
  Play section).
- Flathub. The app builds from source the way Flathub requires, but two
  bundled tools still come prebuilt: Terraform (HashiCorp's release binary,
  where Flathub would build it from its Go source) and five of the Python
  wheels Ansible needs (cryptography, cffi, PyYAML, MarkupSafe and
  charset-normalizer, which have compiled code). A submission would also
  need the app from a git tag rather than this checkout, and a word on
  Terraform's licence (BUSL-1.1, not open source) next to the app's own
  (MIT).
- aarch64.
