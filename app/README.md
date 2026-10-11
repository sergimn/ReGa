# ReGa desktop app

Flutter UI on top of a Rust core, for Linux (Ubuntu), packaged as a Flatpak.

```
app/
  core/          rega_core: provider interface, region picker, AWS catalog, mock provider
  rust/          rega_bridge: the API the UI calls, via flutter_rust_bridge
  lib/           Flutter UI (lib/src/rust is generated, don't edit)
  test/          widget tests against a fake backend (no Rust, no cloud)
  integration_test/  runs the real app against the Rust core
  rust_builder/  cargokit glue that builds rust/ during `flutter build`
  flatpak/       Flatpak manifest, desktop file, metainfo, icon
```

## Providers

Every cloud implements `CloudProvider` in `core/src/provider.rs`. AWS is the
first. Its machine catalog and bucket names are real, but the calls that need
the AWS SDK return `NotImplemented` for now, so the app runs AWS on demo data
(`MockProvider::demo_aws`) and says so on screen.

## Develop

Needs Flutter 3.47.7, a stable Rust toolchain and the GTK build deps
(`sudo apt install clang cmake ninja-build pkg-config libgtk-3-dev liblzma-dev`).

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

No cloud credentials are needed to build or test. See `flatpak/README.md`
for packaging.
