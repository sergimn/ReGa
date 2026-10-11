#!/usr/bin/env bash
# A WireGuard server for tests/tunnel.rs, set up like a ReGa machine
# (10.213.37.1/24, MTU 1400) in its own network namespace, with TCP and UDP
# echo servers on Sunshine's ports. Prints the variables the test reads:
#
#   sudo tests/wireguard/server.sh up > wg-test.env
#   set -a; . ./wg-test.env; set +a
#   REGA_REQUIRE_WG_SERVER=1 cargo test --workspace --test tunnel
#   sudo tests/wireguard/server.sh down
#
# Needs root, iproute2, wireguard-tools and python3. Uses the kernel's
# WireGuard, or wireguard-go (WIREGUARD_GO, default wireguard-go) when the
# kernel has none.
set -euo pipefail
ns=rega-wg-test
here=$(cd "$(dirname "$0")" && pwd)

down() {
  for pid in $(ip netns pids "$ns" 2>/dev/null); do kill "$pid" 2>/dev/null || true; done
  ip link del rega-wg-out 2>/dev/null || true
  ip netns del "$ns" 2>/dev/null || true
}

case "${1:-up}" in
  down) down; exit 0 ;;
  up) down ;;
  *) echo "usage: $0 up|down" >&2; exit 2 ;;
esac

# Room for a burst of video in socket buffers, as on a desktop tuned for
# streaming; the default 208 KiB holds about a hundred packets.
sysctl -q -w net.core.rmem_max=4194304 net.core.wmem_max=4194304

server_key=$(wg genkey)
client_key=$(wg genkey)

# The outer packets cross a veth pair, as they would cross a network.
ip netns add "$ns"
ip -n "$ns" link set lo up
ip link add rega-wg-out type veth peer name eth0 netns "$ns"
ip addr add 198.18.37.1/30 dev rega-wg-out
ip link set rega-wg-out up
ip -n "$ns" addr add 198.18.37.2/30 dev eth0
ip -n "$ns" link set eth0 up

if ! ip -n "$ns" link add wg0 type wireguard 2>/dev/null; then
  ip netns exec "$ns" "${WIREGUARD_GO:-wireguard-go}" wg0 >&2
fi
ip netns exec "$ns" wg set wg0 private-key <(echo "$server_key") listen-port 51820 \
  peer "$(echo "$client_key" | wg pubkey)" allowed-ips 10.213.37.2/32
ip -n "$ns" addr add 10.213.37.1/24 dev wg0
ip -n "$ns" link set wg0 mtu 1400 up

setsid ip netns exec "$ns" python3 "$here/echo.py" 10.213.37.1 </dev/null >/dev/null 2>&1 &
for _ in $(seq 50); do
  if ip netns exec "$ns" ss -Hltn 'sport = 47990' | grep -q .; then break; fi
  sleep 0.1
done

echo "REGA_WG_TEST_ENDPOINT=198.18.37.2:51820"
echo "REGA_WG_TEST_SERVER_PUBLIC_KEY=$(echo "$server_key" | wg pubkey)"
echo "REGA_WG_TEST_CLIENT_PRIVATE_KEY=$client_key"
