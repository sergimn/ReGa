"""TCP and UDP echo servers on Sunshine's ports, for tests/tunnel.rs."""

import socket
import sys
import threading

HOST = sys.argv[1]
TCP_PORTS = [47984, 47989, 47990, 48010]
UDP_PORTS = [47998, 47999, 48000, 48002, 48010]
# Linux constants Python doesn't name everywhere.
IP_MTU_DISCOVER = 10
IP_PMTUDISC_DONT = 0


def udp(port):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    # Like Sunshine on Windows: fragment replies bigger than the tunnel's
    # MTU rather than refuse to send them.
    s.setsockopt(socket.IPPROTO_IP, IP_MTU_DISCOVER, IP_PMTUDISC_DONT)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4 << 20)
    s.bind((HOST, port))
    while True:
        data, peer = s.recvfrom(65535)
        if data.startswith(b"BURST "):
            # "BURST <count> <size>": that many datagrams back to back, each
            # starting with its number, like the packets of a video frame.
            count, size = (int(n) for n in data.split()[1:3])
            for i in range(count):
                s.sendto(i.to_bytes(4, "big") + bytes(size - 4), peer)
        else:
            s.sendto(data, peer)


def echo(conn):
    with conn:
        while data := conn.recv(65536):
            conn.sendall(data)


def tcp(port):
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind((HOST, port))
    s.listen()
    while True:
        conn, _ = s.accept()
        threading.Thread(target=echo, args=(conn,), daemon=True).start()


threads = [threading.Thread(target=udp, args=(p,)) for p in UDP_PORTS]
threads += [threading.Thread(target=tcp, args=(p,)) for p in TCP_PORTS]
for t in threads:
    t.start()
for t in threads:
    t.join()
