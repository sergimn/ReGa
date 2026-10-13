#!/bin/sh
# Stands in for rustup in the Flatpak build, which has one Rust toolchain
# (rust.yml, at CI's RUST_VERSION) and no rustup. cargokit (app/rust_builder)
# drives cargo through rustup, and rust_notices.py finds rustc that way, so
# this answers the calls they make as rustup would with only that toolchain,
# installed as stable. Anything else fails, rather than pass for rustup.
set -eu

triple="${FLATPAK_ARCH:?}-unknown-linux-gnu"
toolchain="stable-$triple"

case "$*" in
  "toolchain list")
    echo "$toolchain (default)"
    ;;
  "target list --toolchain stable --installed" | "target list --toolchain $toolchain --installed")
    echo "$triple"
    ;;
  "run stable "* | "run $toolchain "*)
    shift 2
    exec "$@"
    ;;
  *)
    echo "rustup.sh: the Flatpak build has no rustup, and can't do: rustup $*" >&2
    exit 1
    ;;
esac
