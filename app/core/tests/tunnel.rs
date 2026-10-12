//! The tunnel against a real WireGuard server (the kernel's, or
//! wireguard-go), set up like a ReGa machine by `tests/wireguard/server.sh`.
//! Without the server's variables the test skips, unless
//! `REGA_REQUIRE_WG_SERVER=1` (as in CI) makes that a failure.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use rega_core::stream::tunnel::{
    Forward, Protocol, Tunnel, MOONLIGHT_TCP_PORTS, MOONLIGHT_UDP_PORTS, SUNSHINE_API_PORT,
};
use rega_core::stream::WireGuardPeer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

fn server() -> Option<WireGuardPeer> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let (Some(endpoint), Some(server_key), Some(client_key)) = (
        var("REGA_WG_TEST_ENDPOINT"),
        var("REGA_WG_TEST_SERVER_PUBLIC_KEY"),
        var("REGA_WG_TEST_CLIENT_PRIVATE_KEY"),
    ) else {
        assert!(
            var("REGA_REQUIRE_WG_SERVER").as_deref() != Some("1"),
            "REGA_REQUIRE_WG_SERVER=1 but no WireGuard test server; run tests/wireguard/server.sh"
        );
        eprintln!("skipped: no WireGuard test server (see tests/wireguard/server.sh)");
        return None;
    };
    Some(WireGuardPeer {
        endpoint: endpoint.parse().expect("REGA_WG_TEST_ENDPOINT"),
        server_public_key: server_key,
        client_private_key: client_key,
        client_address: Ipv4Addr::new(10, 213, 37, 2),
        server_address: Ipv4Addr::new(10, 213, 37, 1),
        mtu: 1400,
    })
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

async fn udp_echo(socket: &UdpSocket, payload: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; 65536];
    for _ in 0..3 {
        socket.send(payload).await.unwrap();
        let read = tokio::time::timeout(Duration::from_secs(2), socket.recv(&mut buf));
        if let Ok(Ok(n)) = read.await {
            return buf[..n].to_vec();
        }
    }
    panic!("no echo for a {}-byte datagram", payload.len());
}

async fn tcp_echo(addr: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = stream.into_split();
    let data = payload.to_vec();
    let writer = tokio::spawn(async move {
        write.write_all(&data).await.unwrap();
        write.shutdown().await.unwrap();
    });
    let mut back = Vec::new();
    let reading = read.read_to_end(&mut back);
    tokio::time::timeout(Duration::from_secs(60), reading)
        .await
        .expect("TCP echo timed out")
        .unwrap();
    writer.await.unwrap();
    back
}

/// Echoes `payload` on a connection that stays open.
async fn quiet_echo(stream: &mut TcpStream, payload: &[u8]) -> Vec<u8> {
    stream.write_all(payload).await.unwrap();
    let mut back = vec![0u8; payload.len()];
    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut back))
        .await
        .expect("TCP echo timed out")
        .unwrap();
    back
}

#[tokio::test(flavor = "multi_thread")]
async fn moonlight_ports_reach_a_real_wireguard_server() {
    let Some(peer) = server() else { return };
    let tunnel = Tunnel::open(&peer).expect("Moonlight's ports are free on this computer");
    tunnel.wait_until_up(Duration::from_secs(15)).await.unwrap();

    // TCP: a megabyte each way, through smoltcp's flow control, on every
    // Moonlight port and the API port the app uses.
    let big = pattern(1 << 20, 7);
    let api = tunnel.local_addr(Protocol::Tcp, SUNSHINE_API_PORT).unwrap();
    assert_eq!(api.ip(), Ipv4Addr::LOCALHOST);
    assert_ne!(api.port(), SUNSHINE_API_PORT);

    // A connection that goes quiet, like Moonlight's pairing request while
    // the user types the PIN. Checked at the end, past the tunnel's
    // 30-second TCP timeout.
    let mut quiet = TcpStream::connect(api).await.unwrap();
    let quiet_since = Instant::now();
    assert_eq!(quiet_echo(&mut quiet, b"hello").await, b"hello");

    assert_eq!(tcp_echo(api, &big).await, big);
    for port in MOONLIGHT_TCP_PORTS {
        let local = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        assert_eq!(
            tcp_echo(local, b"GET /serverinfo").await,
            b"GET /serverinfo"
        );
    }
    // Several connections at once.
    let together = futures::future::join_all((0..8u8).map(|i| {
        let data = pattern(100_000, i);
        async move { (tcp_echo(api, &data).await, data) }
    }))
    .await;
    assert!(together.iter().all(|(back, sent)| back == sent));

    // UDP on every Moonlight port, including Moonlight's 1392-byte video
    // packets, which only fit an MTU-1400 tunnel in fragments, both ways.
    for port in MOONLIGHT_UDP_PORTS {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket.connect(("127.0.0.1", port)).await.unwrap();
        for len in [1, 100, 1372, 1373, 1392, 1500, 9000] {
            let payload = pattern(len, port as u8);
            assert_eq!(
                udp_echo(&socket, &payload).await,
                payload,
                "{len} bytes to {port}"
            );
        }
        // One copy of each reply, and only from the port it went to.
        let mut buf = [0u8; 16];
        let stray = tokio::time::timeout(Duration::from_millis(200), socket.recv(&mut buf));
        assert!(stray.await.is_err(), "a stray datagram on port {port}");
    }

    // A burst like a video keyframe: the machine sends 300 video packets
    // back to back, each in two fragments.
    // Room for the whole burst, so a reader that falls behind on a busy CI
    // runner doesn't drop it itself.
    let video = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).unwrap();
    video.set_recv_buffer_size(4 << 20).unwrap();
    video.set_nonblocking(true).unwrap();
    video
        .bind(&SocketAddr::from((Ipv4Addr::LOCALHOST, 0)).into())
        .unwrap();
    let video = UdpSocket::from_std(video.into()).unwrap();
    video.connect(("127.0.0.1", 47998)).await.unwrap();
    let count = 300;
    let started = Instant::now();
    video
        .send(format!("BURST {count} 1392").as_bytes())
        .await
        .unwrap();
    let mut seen = vec![false; count];
    let mut buf = vec![0u8; 2048];
    while let Ok(Ok(n)) =
        tokio::time::timeout(Duration::from_millis(500), video.recv(&mut buf)).await
    {
        assert_eq!(n, 1392);
        seen[u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize] = true;
    }
    let back = seen.iter().filter(|s| **s).count();
    eprintln!("burst: {back}/{count} in {:?}", started.elapsed());
    assert!(back * 100 >= count * 95, "only {back} of {count} arrived");

    tokio::time::sleep_until((quiet_since + Duration::from_secs(40)).into()).await;
    assert_eq!(
        quiet_echo(&mut quiet, b"still here").await,
        b"still here",
        "a quiet connection was cut"
    );
    drop(quiet);

    tunnel.close().await;
    // The ports are free again for the next Play.
    let tunnel = Tunnel::open_with(
        &peer,
        &[Forward::tcp(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 47984)),
            47984,
        )],
    )
    .unwrap();
    tunnel.wait_until_up(Duration::from_secs(15)).await.unwrap();
    let local = SocketAddr::from((Ipv4Addr::LOCALHOST, 47984));
    assert_eq!(tcp_echo(local, b"again").await, b"again");
}
