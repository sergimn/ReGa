# Flatpak

ReGa ships as the Flatpak `io.github.sergimn.rega`, for x86_64, on the
`org.freedesktop.Platform` 25.08 runtime from Flathub.

## Install

Every CI run of the App workflow builds `rega.flatpak`; download the
`rega-flatpak` artifact from the run's page. On Ubuntu:

```sh
sudo apt install flatpak
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
unzip rega-flatpak.zip
flatpak install --user rega.flatpak     # also pulls the runtime from Flathub
flatpak run io.github.sergimn.rega
```

## What's inside

| Path in the sandbox | What |
| --- | --- |
| `/app/lib/rega`, `/app/bin/rega` | The Flutter release bundle with the Rust core |
| `/app/bin/terraform` | Terraform, the version `.github/workflows/terraform.yml` tests with |
| `/app/bin/ansible-playbook` and friends | Ansible with pywinrm, at the versions in `infra/ansible/requirements.lock`; the app runs the playbook it carries with it |
| `/app/lib/python3.13/site-packages/ansible_collections` | The collections in `infra/ansible/requirements.yml`, found without any config |
| `/app/share/licenses/io.github.sergimn.rega` | ReGa's licence (MIT), and Terraform's (BUSL-1.1) in `terraform/` |

Permissions: network, display (Wayland, or X11 as a fallback), GPU, and
`~/.aws` read-only for credentials and profiles, with the SSO token cache
writable so an SSO sign-in can refresh it. Nothing else in your home folder.

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

The manifest packages the Flutter bundle that `flutter build linux` makes,
rather than building the app from source inside flatpak-builder. So build
that first, from `app/`:

```sh
flutter build linux --release
flatpak-builder --user --install --install-deps-from=flathub --force-clean \
  build/flatpak flatpak/io.github.sergimn.rega.yml
dbus-run-session -- flatpak/smoke-test.sh
```

The smoke test checks the installed app: Terraform and Ansible run in the
sandbox, the Windows collections are there, the playbooks in
`infra/ansible` pass a syntax check, and the app draws its window (on demo data, on a private
Xvfb display over X11, whatever your desktop runs). It needs Xvfb and
xdotool.

## Pins

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

## Not done yet

- Launching Moonlight from inside the sandbox. The Moonlight Flatpak is a
  separate app, so starting it needs either a host permission or bundling
  Moonlight here; that's a decision for when the app starts streams.
- Flathub. It would need the app built from source inside flatpak-builder
  (Flutter, Dart packages and Cargo crates as offline sources), a review of
  the `~/.aws` permission, and a word on the bundled Terraform's licence
  (BUSL-1.1, not open source) next to the app's own (MIT).
- The licence notices of the Rust crates built into the app (generated with
  something like cargo-about). Flutter already bundles those of the Dart
  packages and the engine, in `flutter_assets/NOTICES.Z`.
- aarch64.
