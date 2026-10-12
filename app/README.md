# ReGa desktop app

Flutter UI on top of a Rust core, for Linux (Ubuntu), packaged as a Flatpak.

```
app/
  core/          rega_core: provider interface, region picker, AWS provider, mock provider
  rust/          rega_bridge: the API the UI calls, via flutter_rust_bridge
  lib/           Flutter UI (lib/src/rust is generated, don't edit)
  test/          widget tests against a fake backend (no Rust, no cloud)
  integration_test/  runs the real app against the Rust core
  rust_builder/  cargokit glue that builds rust/ during `flutter build`
  flatpak/       Flatpak manifest, desktop file, metainfo, pin check, smoke test
  assets/        app icon (SVG and PNG sizes) and the palette swatch
```

Colours come from `RegaColors` in `lib/theme.dart`; use those rather than
literal colours in widgets.

## Providers

Every cloud implements `CloudProvider` in `core/src/provider.rs`. AWS is the
first (`core/src/aws/`):

- Credentials are only ever the ones the user enters in the app (the
  provider's page has Enter credentials, then Change credentials and Sign
  out). Nothing is picked up from the computer: no environment variables,
  no `~/.aws` or other CLI configuration, no instance metadata, even when
  the computer has credentials there. For AWS that is an IAM user's access
  key ID and secret access key.
  - Each provider says which fields it needs (`Connector` in
    `core/src/provider.rs`); the form is built from that, so another cloud
    only needs a `Connector` of its own.
  - Save signs in first and only keeps credentials the cloud accepted,
    replacing what was saved before. The page then says who it's signed in
    as (for AWS, the IAM user and account), so a wrong account shows.
  - They are kept in the system keyring (`core/src/credentials.rs`, with
    `oo7`): the desktop's Secret Service outside a sandbox, and inside the
    Flatpak a file in the app's data folder, encrypted with a key from the
    Secret portal (GNOME Keyring provides it on Ubuntu). If they can't be
    saved, the app stays signed in until it quits and says so.
  - The AWS SDK is set up by hand from the key (the app doesn't use
    `aws-config`, which is what reads the environment and `~/.aws`).
    Terraform gets the key as environment variables, with
    `AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE` pointed at
    `/dev/null` and instance metadata off, so its AWS provider can't find
    anything else either. Ansible never talks to AWS.
  - Without saved credentials, or when signing in with them fails, the app
    shows demo data (`MockProvider::demo_aws`) and says why; Reconnect tries
    the saved ones again.
- Regions, GPU quotas (Service Quotas), spot prices and instance offerings
  come from the AWS APIs; on-demand prices from the Price List API.
- The first time it lists machines, the app creates `rega-tfstate-<account>`
  (versioned, with old versions expiring after 30 days) and
  `rega-games-<account>` in eu-north-1, encrypted and tagged. It only checks
  Block Public Access and refuses a bucket that doesn't have it on.
- Launch, start, stop, resize and destroy run the Terraform module in
  `infra/terraform/aws`, which is compiled into the app. Each machine has its
  own state in the state bucket (the source of truth, so every device sees
  the same machines), next to a record of its settings and its RSA key. Each
  device keeps a working copy under `$XDG_DATA_HOME/rega/aws/<account>/`.
  Terraform comes from `REGA_TERRAFORM` or `PATH` (the Flatpak bundles it).
- One change per machine at a time, and one machine up at a time: launch,
  start and resize refuse while another ReGa machine is running or being
  started, here or on another device. A machine with a change running
  (seen through its Terraform state lock) shows as busy and offers no
  actions until it's done; the app rechecks it every 15 seconds. Two
  devices that launch at about the same time, before either one's Terraform
  has taken its state lock, can still both get through, since nothing in S3
  serialises them yet.
- If the app quits mid-change, Terraform keeps going and finishes (in the
  Flatpak too). A run that was killed leaves its state lock behind; the next
  change to that machine removes it when this computer took it and the run
  is gone, or when it is over two hours old. A run started by an earlier
  Flatpak sandbox can't be seen from a new one, so it counts as gone only
  after a reboot.
- Leftovers are resources tagged `Project=ReGa` that no machine's Terraform
  state names, such as an instance a killed run made. They show up under
  "Leftover resources", with a button to delete them.
- The idle-stop alarm needs the service-linked role
  `AWSServiceRoleForCloudWatchEvents`, which `rega-claude` can't create. An
  admin creates it once per account with
  `aws iam create-service-linked-role --aws-service-name events.amazonaws.com`;
  until then launches fail with that command in the error.
- After Terraform, the app sets the machine up with the Ansible playbook in
  `infra/ansible` (`core/src/ansible.rs`, behind `MachineSetup` in
  `core/src/setup.rs`). The playbook is compiled into the app and written to
  the machine's folder before each run; `ansible-playbook` comes from
  `REGA_ANSIBLE_PLAYBOOK` or `PATH` (the Flatpak bundles it), with the `ansible.windows` and
  `community.windows` collections where Ansible finds them
  (`ANSIBLE_COLLECTIONS_PATH` passes through). Launch, start and switch
  check for it before anything boots.
  - It waits for Windows to make its Administrator password (up to 30
    minutes), opens WinRM (5986) to this device's IP only, and runs
    `site.yml`. The password reaches Ansible through an environment
    variable, never a file. Sunshine's and the desktop account's passwords
    are generated once and kept in the state bucket
    (`machines/<id>.secrets.json`), so every device uses the same ones. Each
    run's full output, stderr included, is in the machine's folder
    (`setup.log`, `restore.log`, `final-sync.log`).
  - While it sets a machine up or saves its games, the app keeps
    `machines/<id>.setup` in the state bucket fresh (every minute), so
    other devices show the machine as busy. A marker left by an app that
    quit counts for 5 minutes.
  - CPU first: an on-demand GPU launch can run on the smallest CPU type
    with 4 GB of RAM or more while it installs everything and restores the
    game library (`agent.yml`, `WaitRestore`), then switch to the GPU type in
    one Terraform run that keeps D:, and run the setup again for the GPU
    driver. The UI offers this as a toggle, on by default.
  - Switching a running machine restarts it as the new type and sets it up
    again; a stopped one stays stopped.
  - Stop and destroy first have the machine upload its game library
    (`agent.yml`, `FinalSync`). Stop drops D: only when the machine's
    `rega/machines/<id>/final-sync.json` in the library bucket says the
    upload worked and finished after the instance's last start (the app
    deletes the old record first). A failed upload doesn't block the stop;
    D: just stays, and the machine says so until a later stop saves its
    games.
  - Destroy refuses when the machine's D: may hold games the library
    doesn't, and says why; the app then asks whether to destroy it anyway,
    which doesn't try the upload again. That includes the D: of a spot
    machine AWS took back, which outlives its instance.
  - Until a setup has started on a machine (the record's
    `setup_started`), nothing can have put games on its D:, so stop and
    destroy skip the upload and treat D: as empty. Once one has started,
    even if it failed, they upload as usual.
  - Idle stop: after 15 minutes without a stream the machine uploads its
    games and shuts itself down, which EC2 turns into a stop. The app checks
    running machines every minute, and finishes the stop of one that
    stopped itself (record says running, instance stopped, record last
    saved over 10 minutes ago so it isn't another device starting it, and
    no change or setup marker on it). That drops D: by the same rule. Spot
    machines are never treated this way.

## Play

A running machine has a **Play** button (`core/src/stream/`, with no cloud
in it; the provider only says where the machine is, through
`CloudProvider::stream_target`). It:

1. Connects. With WireGuard on, the app runs its own userspace WireGuard
   client (`tunnel.rs`, on boringtun and smoltcp): no root, no kernel module
   and no system VPN, so it works in the Flatpak. It forwards Moonlight's
   ports on 127.0.0.1 (TCP 47984, 47989 and 48010; UDP 47998, 47999, 48000,
   48002 and 48010) to the machine's tunnel address, plus a free local port
   to Sunshine's API (47990). A port that's taken, say by a local Sunshine,
   fails with its number. Moonlight's 1392-byte video packets don't fit the
   MTU-1400 tunnel, so the tunnel wraps UDP in IPv4 itself and joins
   fragments again. With WireGuard off, the app first points the Sunshine
   ports at this device's current IP and Moonlight goes to the public IP.
2. Pairs Moonlight with the machine's Sunshine, through Sunshine's API
   (`sunshine.rs`), with the login from the machine's secrets.
   - Where the app can run Moonlight (`moonlight.rs`: `REGA_MOONLIGHT`,
     then `moonlight` or `moonlight-qt` on `PATH`, then the Flathub app
     outside a sandbox), pairing is automatic: the app picks a PIN, runs
     `moonlight pair` with it and hands Sunshine the same PIN.
   - Inside ReGa's Flatpak there's no Moonlight to run, so Play ends in a
     dialog: add the host it shows in Moonlight, then type the PIN Moonlight
     shows. With WireGuard on, the host is always 127.0.0.1, so that's once
     per machine.
3. Starts `moonlight stream <host> Desktop` (automatic only).

While the tunnel is up the machine says "Streaming through WireGuard" and
offers Disconnect. Stop, destroy and switching a running machine close it
first, a machine that stopped itself has it closed, and quitting the app
asks first. One WireGuard key per machine, generated once and kept with the
machine's secrets in the state bucket, is shared by every device; one
machine streams to one device at a time anyway.

All AWS calls stay inside the `rega-claude` IAM policy (see the AWS setup
guide). Calls go through the `AwsApi` trait, so the provider's logic is
tested against a fake account (`core/src/aws/fake.rs`).

## Develop

Needs Flutter 3.47.7, a stable Rust toolchain and the GTK build deps
(`sudo apt install clang cmake ninja-build pkg-config libgtk-3-dev liblzma-dev`,
plus `xvfb` for the integration test).

```sh
cd app
cargo test --workspace         # Rust unit and integration tests
flutter test                   # widget tests
flutter run -d linux           # builds the Rust core too
xvfb-run -a flutter test integration_test -d linux
```

After changing anything in `rust/src/api/`, regenerate the bindings:

```sh
cargo install flutter_rust_bridge_codegen --version 2.13.0 --locked
flutter_rust_bridge_codegen generate
```

The tunnel test (`core/tests/tunnel.rs`) needs a real WireGuard server.
`core/tests/wireguard/server.sh` (root, `wireguard-tools`, `python3`) sets
one up like a machine in a network namespace, with echo servers on
Sunshine's ports; without its variables the test skips, unless
`REGA_REQUIRE_WG_SERVER=1`, as in CI:

```sh
sudo core/tests/wireguard/server.sh up > /tmp/wg-test.env
set -a; . /tmp/wg-test.env; set +a
REGA_REQUIRE_WG_SERVER=1 cargo test --workspace --test tunnel
sudo core/tests/wireguard/server.sh down
```

It uses the kernel's WireGuard, or `wireguard-go` when the kernel has none.

The keyring test of the credentials store (`core/src/credentials.rs`) saves
to a real keyring, so it only runs with `REGA_TEST_KEYRING=1`.
`core/tests/with-keyring.sh` (needs `gnome-keyring`) runs a command with a
throwaway GNOME Keyring and sets that, as CI does:

```sh
dbus-run-session -- core/tests/with-keyring.sh cargo test --workspace
```

No cloud credentials are needed to build or test. `REGA_DEMO=1` runs the
app (and the integration test, as CI does) on demo data without touching
the keyring. Read-only checks against a real account take an access key
from variables of their own, never the usual `AWS_*` ones:

```sh
REGA_TEST_AWS_ACCESS_KEY_ID=... REGA_TEST_AWS_SECRET_ACCESS_KEY=... \
  cargo test -p rega_core --test aws_live -- --ignored --nocapture
```

See `flatpak/README.md` for packaging.
