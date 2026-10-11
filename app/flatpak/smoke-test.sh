#!/usr/bin/env bash
# Checks an installed ReGa Flatpak from the outside: the tools it bundles
# work inside the sandbox, and the app starts and opens its window.
#
#   dbus-run-session -- app/flatpak/smoke-test.sh
#
# Needs flatpak, Xvfb and xdotool. Uses $DISPLAY when set, otherwise starts
# its own Xvfb. No cloud credentials: the app runs on demo data.
set -euo pipefail

app=io.github.sergimn.rega
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
in_app() { flatpak run --command="$1" "$app" "${@:2}"; }

flatpak info "$app" >/dev/null || fail "$app isn't installed"

# Terraform, at the version the manifest pins.
terraform=$(sed -n 's|.*releases\.hashicorp\.com/terraform/\([0-9.]*\)/.*|\1|p' "$here/$app.yml")
out=$(in_app terraform version)
grep -q "^Terraform v$terraform\$" <<<"$out" || fail "expected Terraform $terraform, got: $out"
ok "Terraform $terraform"

# Ansible with its WinRM client and the Windows collections.
out=$(in_app ansible-playbook --version)
ok "$(head -n1 <<<"$out")"
in_app python3 -c 'import winrm, requests_ntlm, spnego' || fail "pywinrm doesn't import"
ok "pywinrm and NTLM import"
out=$(in_app ansible-galaxy collection list 2>/dev/null)
for collection in ansible.windows community.windows; do
  grep -q "^$collection " <<<"$out" || fail "$collection isn't bundled: $out"
done
ok "Windows collections: $(awk '/^(ansible|community)\.windows / { print $1, $2 }' <<<"$out" | paste -sd, -)"

# Ansible can run a module inside the sandbox (it needs a writable temp dir),
# and the repo's playbook resolves every module and role it names.
out=$(in_app ansible localhost -m ansible.builtin.ping 2>/dev/null)
grep -q '"ping": "pong"' <<<"$out" || fail "ansible can't run a local module: $out"
ok "ansible runs a module locally"
flatpak run --filesystem="$repo/infra/ansible:ro" --cwd="$repo/infra/ansible" \
  --command=ansible-playbook "$app" -i inventory.example.yml site.yml --syntax-check >/dev/null \
  || fail "the playbook doesn't pass a syntax check with the bundled Ansible"
ok "infra/ansible/site.yml passes a syntax check"

# The app opens its window and keeps running.
if [ -z "${DISPLAY:-}" ]; then
  Xvfb :91 -screen 0 1280x800x24 -nolisten tcp >/dev/null 2>&1 &
  xvfb=$!
  export DISPLAY=:91
  sleep 2
fi
log=$(mktemp)
flatpak run --env=REGA_DEMO=1 "$app" >"$log" 2>&1 &
pid=$!
cleanup() {
  kill "$pid" 2>/dev/null || true
  if [ -n "${xvfb:-}" ]; then kill "$xvfb" 2>/dev/null || true; fi
  rm -f "$log"
}
trap cleanup EXIT
for _ in $(seq 60); do
  xdotool search --name '^ReGa$' >/dev/null 2>&1 && break
  kill -0 "$pid" 2>/dev/null || { cat "$log"; fail "the app exited before opening a window"; }
  sleep 1
done
xdotool search --name '^ReGa$' >/dev/null 2>&1 || { cat "$log"; fail "no ReGa window after 60 seconds"; }
sleep 5
kill -0 "$pid" 2>/dev/null || { cat "$log"; fail "the app exited after opening its window"; }
ok "the app opens its window and keeps running"
