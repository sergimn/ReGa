#!/bin/sh
# Runs a command with a throwaway GNOME Keyring as the session's Secret
# Service, for the test of the credentials store (REGA_TEST_KEYRING=1):
#
#   dbus-run-session -- core/tests/with-keyring.sh cargo test --workspace
#
# Needs gnome-keyring. The keyring lives in a temporary folder, unlocked with
# a throwaway password, and goes when the D-Bus session ends.
set -eu
data=$(mktemp -d)
trap 'rm -rf "$data"' EXIT
mkdir -p "$data/keyrings"
# The login keyring is the default one, as on a desktop.
printf login >"$data/keyrings/default"
export XDG_DATA_HOME="$data"
# Captured first, so set -e stops here if the daemon doesn't start.
daemon=$(printf throwaway | gnome-keyring-daemon --unlock --components=secrets)
eval "$daemon"
export GNOME_KEYRING_CONTROL
REGA_TEST_KEYRING=1 "$@"
