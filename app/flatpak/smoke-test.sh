#!/usr/bin/env bash
# Checks an installed ReGa Flatpak from the outside: the tools it bundles
# work inside the sandbox, and the app starts and draws its window.
#
#   dbus-run-session -- app/flatpak/smoke-test.sh
#
# Needs flatpak, Xvfb and xdotool. The app runs on a private Xvfb display,
# over X11, so it never touches your desktop. No cloud credentials: the app
# runs on demo data.
set -euo pipefail

app=io.github.sergimn.rega
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
in_app() { flatpak run --command="$1" "$app" "${@:2}"; }

flatpak info "$app" >/dev/null || fail "$app isn't installed"

# Terraform's licence, which asks to ship with every copy of it.
in_app test -s "/app/share/licenses/$app/terraform/LICENSE.txt" || fail "Terraform's licence isn't installed"
ok "Terraform's licence is installed"

# Terraform, at the version the manifest pins.
terraform=$(sed -n 's|.*releases\.hashicorp\.com/terraform/\([0-9.]*\)/.*|\1|p' "$here/$app.yml")
out=$(in_app terraform version 2>&1) || fail "terraform doesn't run: $out"
grep -q "^Terraform v$terraform\$" <<<"$out" || fail "expected Terraform $terraform, got: $out"
ok "Terraform $terraform"

# Ansible with its WinRM client and the Windows collections.
out=$(in_app ansible-playbook --version 2>&1) || fail "ansible-playbook doesn't run: $out"
ok "$(head -n1 <<<"$out")"
out=$(in_app python3 -c 'import winrm, requests_ntlm, spnego' 2>&1) || fail "pywinrm doesn't import: $out"
ok "pywinrm and NTLM import"
out=$(in_app ansible-galaxy collection list 2>&1) || fail "ansible-galaxy can't list collections: $out"
for collection in ansible.windows community.windows; do
  grep -q "^$collection " <<<"$out" || fail "$collection isn't bundled: $out"
done
ok "Windows collections: $(awk '/^(ansible|community)\.windows / { print $1, $2 }' <<<"$out" | paste -sd, -)"

# Ansible can run a module inside the sandbox (it needs a writable temp dir),
# and the playbooks the app runs resolve every module and role they name.
out=$(in_app ansible localhost -m ansible.builtin.ping 2>&1) || fail "ansible can't run a local module: $out"
grep -q '"ping": "pong"' <<<"$out" || fail "ansible can't run a local module: $out"
ok "ansible runs a module locally"
out=$(flatpak run --filesystem="$repo/infra/ansible:ro" --cwd="$repo/infra/ansible" \
  --command=ansible-playbook "$app" -i inventory.example.yml site.yml agent.yml --syntax-check 2>&1) \
  || fail "the playbooks don't pass a syntax check with the bundled Ansible: $out"
ok "infra/ansible/site.yml and agent.yml pass a syntax check"

# The app draws its window and keeps running. The runner shows the window
# only on Flutter's first frame, which comes after the Rust core loads.
log=$(mktemp)
display=$(mktemp)
pid=
xvfb=
cleanup() {
  if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
  if [ -n "$xvfb" ]; then kill "$xvfb" 2>/dev/null || true; fi
  rm -f "$log" "$display"
}
trap cleanup EXIT
Xvfb -displayfd 3 -screen 0 1280x800x24 -nolisten tcp 3>"$display" >/dev/null 2>&1 &
xvfb=$!
for _ in $(seq 50); do
  [ -s "$display" ] && break
  sleep 0.2
done
[ -s "$display" ] || fail "Xvfb didn't start"
DISPLAY=":$(head -n1 "$display")"
export DISPLAY
unset WAYLAND_DISPLAY

window() { xdotool search --onlyvisible --name '^ReGa$' >/dev/null 2>&1; }
flatpak run --nosocket=wayland --env=REGA_DEMO=1 "$app" >"$log" 2>&1 &
pid=$!
for _ in $(seq 60); do
  window && break
  kill -0 "$pid" 2>/dev/null || { cat "$log"; fail "the app exited before drawing its window"; }
  sleep 1
done
window || { cat "$log"; fail "no ReGa window after 60 seconds"; }
sleep 5
kill -0 "$pid" 2>/dev/null || { cat "$log"; fail "the app exited after drawing its window"; }
ok "the app draws its window and keeps running"
