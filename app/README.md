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
  flatpak/       Flatpak manifest, desktop file, metainfo
  assets/        app icon (SVG and PNG sizes) and the palette swatch
```

Colours come from `RegaColors` in `lib/theme.dart`; use those rather than
literal colours in widgets.

## Providers

Every cloud implements `CloudProvider` in `core/src/provider.rs`. AWS is the
first (`core/src/aws/`):

- Credentials come from the standard AWS chain (environment variables, then
  `~/.aws` profiles). Without any, the app shows demo data
  (`MockProvider::demo_aws`) and says why; Reconnect tries again.
- Regions, GPU quotas (Service Quotas), spot prices and instance offerings
  come from the AWS APIs; on-demand prices from the Price List API.
- On first use the app creates `rega-tfstate-<account>` (versioned) and
  `rega-games-<account>` in eu-north-1, encrypted and tagged. It only checks
  Block Public Access and refuses a bucket that doesn't have it on.
- Launch, start, stop, resize and destroy run the Terraform module in
  `infra/terraform/aws`, which is compiled into the app. Each machine has its
  own state in the state bucket (the source of truth, so every device sees
  the same machines), next to a record of its settings and its RSA key. Each
  device keeps a working copy under `$XDG_DATA_HOME/rega/aws/<account>/`.
  Terraform comes from `REGA_TERRAFORM` or `PATH` (the Flatpak bundles it).
- Leftovers tagged `Project=ReGa` that belong to no machine show up under
  "Leftover resources", with a button to delete them.
- What runs on a machine after Terraform (the Ansible playbook in
  `infra/ansible`) plugs in through `MachineSetup` in `core/src/setup.rs`:
  `after_start` runs after every launch and start, and `before_stop` decides
  whether the game library disk (D:) is safe to drop. Until it's wired up,
  stopping keeps D:.

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

No cloud credentials are needed to build or test. With credentials set up,
set `REGA_DEMO=1` to run the app (and the integration test) on demo data
anyway. Read-only checks against a real account:

```sh
cargo test -p rega_core --test aws_live -- --ignored --nocapture
```

See `flatpak/README.md` for packaging.
