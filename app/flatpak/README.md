# Flatpak

ReGa ships as the Flatpak `io.github.sergimn.rega`, for x86_64, on the
`org.freedesktop.Platform` 25.08 runtime from Flathub.

## Install

Every CI run of the App workflow builds `rega.flatpak` (the `rega-flatpak`
artifact). On Ubuntu:

```sh
sudo apt install flatpak
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user rega.flatpak     # also pulls the runtime from Flathub
flatpak run io.github.sergimn.rega
```

## What's inside

| Path in the sandbox | What |
| --- | --- |
| `/app/lib/rega`, `/app/bin/rega` | The Flutter release bundle with the Rust core |
| `/app/bin/terraform` | Terraform, the version `.github/workflows/terraform.yml` tests with |
| `/app/bin/ansible-playbook` and friends | Ansible with pywinrm, at the versions in `infra/ansible/requirements.txt` |
| `/app/lib/python3.13/site-packages/ansible_collections` | The collections in `infra/ansible/requirements.yml`, found without any config |

Permissions: network, display (Wayland, or X11 as a fallback), GPU, and
`~/.aws` read-only for credentials and profiles, with the SSO token cache
writable so an SSO sign-in can refresh it. Nothing else in your home folder.

Things to know when the app runs these tools:

- Terraform and Ansible run inside the sandbox, so quitting the app stops a
  run that is still going. The app asks before quitting mid-change, and the
  next change to that machine clears the Terraform lock the run left.
- The AWS Terraform provider isn't bundled; Terraform downloads it on first
  use into the app's cache.
- Inside the sandbox, `$HOME` is a scratch folder that is emptied when the
  app quits. That suits `~/.ansible` (Ansible's temp files) and
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
sandbox, the Windows collections are there, `infra/ansible/site.yml` passes
a syntax check, and the app opens its window (on demo data, under Xvfb when
there is no display). It needs Xvfb and xdotool.

## Pins

`ansible.yml` is generated: every wheel Ansible needs for the runtime's
Python, and the Galaxy collections, each with its URL and sha256. After
changing `infra/ansible/requirements.txt` or `requirements.yml`, or moving
to a runtime with a different Python, regenerate it:

```sh
python3 flatpak/pins.py update
```

`python3 flatpak/pins.py check` runs in CI and fails when the manifest no
longer matches the repo's pins, Terraform included.

## Not done yet

- The app doesn't run Ansible yet. When it does, the playbook can ship the
  way the Terraform module does (compiled into the app), and run with
  `/app/bin/ansible-playbook`.
- Launching Moonlight from inside the sandbox. The Moonlight Flatpak is a
  separate app, so starting it needs either a host permission or bundling
  Moonlight here; that's a decision for when the app starts streams.
- Flathub. It would need the app built from source inside flatpak-builder
  (Flutter, Dart packages and Cargo crates as offline sources), a license,
  and review of the `~/.aws` permission.
- aarch64.
