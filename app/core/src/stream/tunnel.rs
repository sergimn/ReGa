//! A userspace WireGuard client that forwards ports on 127.0.0.1 to the
//! machine's tunnel address, so Moonlight reaches Sunshine with no root, no
//! kernel module and no system VPN. That also works inside the Flatpak.
//!
//! WireGuard itself is boringtun's. TCP (Moonlight's HTTPS, HTTP and RTSP,
//! and the app's own Sunshine API calls) runs through smoltcp. UDP, which
//! carries the video, is wrapped in IPv4 by hand ([`super::ipv4`]): each
//! datagram goes straight from the local socket into the tunnel and back,
//! with nothing queued in between, and fragments are joined up again.
//!
//! The tunnel runs on its own Tokio runtime on its own threads. Dropping the
//! [`Tunnel`] stops it.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use boringtun::noise::errors::WireGuardError;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Checksum, ChecksumCapabilities, Device, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Notify};

use super::ipv4::{self, Header, Reassembler};
use super::WireGuardPeer;

/// TCP ports Moonlight connects to: HTTPS, HTTP and RTSP.
pub const MOONLIGHT_TCP_PORTS: [u16; 3] = [47984, 47989, 48010];
/// UDP ports Moonlight streams from: video, control, audio, microphone and
/// RTSP over UDP.
pub const MOONLIGHT_UDP_PORTS: [u16; 5] = [47998, 47999, 48000, 48002, 48010];
/// Sunshine's web UI and API, which the app pairs Moonlight through.
pub const SUNSHINE_API_PORT: u16 = 47990;
const KEEPALIVE_SECONDS: u16 = 25;
/// How often boringtun's timers run: handshakes, retries and keepalives.
const TIMER_TICK: Duration = Duration::from_millis(100);
const MAX_DATAGRAM: usize = 65536;
/// Socket buffer each way for UDP: a 1080p keyframe is a few hundred
/// packets arriving at once.
const UDP_BUFFER: usize = 4 << 20;
/// Bytes buffered each way for each TCP connection.
const TCP_BUFFER: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    Tcp,
    Udp,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Protocol::Tcp => "TCP",
            Protocol::Udp => "UDP",
        })
    }
}

/// One forwarded port: `local` on this device to `remote_port` on the
/// machine's tunnel address. A local port of 0 picks a free one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forward {
    pub protocol: Protocol,
    pub local: SocketAddr,
    pub remote_port: u16,
}

impl Forward {
    pub fn tcp(local: SocketAddr, remote_port: u16) -> Self {
        Self {
            protocol: Protocol::Tcp,
            local,
            remote_port,
        }
    }

    pub fn udp(local: SocketAddr, remote_port: u16) -> Self {
        Self {
            protocol: Protocol::Udp,
            local,
            remote_port,
        }
    }
}

/// What Moonlight needs, on Moonlight's own ports on 127.0.0.1, plus the
/// Sunshine API on a free port for the app.
pub fn moonlight_forwards() -> Vec<Forward> {
    let local = |port| SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut forwards: Vec<Forward> = MOONLIGHT_TCP_PORTS
        .iter()
        .map(|&p| Forward::tcp(local(p), p))
        .chain(
            MOONLIGHT_UDP_PORTS
                .iter()
                .map(|&p| Forward::udp(local(p), p)),
        )
        .collect();
    forwards.push(Forward::tcp(local(0), SUNSHINE_API_PORT));
    forwards
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TunnelError {
    #[error(
        "{protocol} port {port} on this computer is already in use, probably by a Sunshine or \
         another streaming host running here. Stop it and try again"
    )]
    PortInUse { protocol: Protocol, port: u16 },
    #[error("couldn't open {protocol} port {port} on this computer: {reason}")]
    Bind {
        protocol: Protocol,
        port: u16,
        reason: String,
    },
    #[error("bad WireGuard key: {0}")]
    BadKey(String),
    #[error("couldn't start the WireGuard tunnel: {0}")]
    Start(String),
    #[error("the machine didn't answer over WireGuard within {0} seconds")]
    NoHandshake(u64),
}

/// A new WireGuard private key, base64 like `wg genkey` prints it.
pub fn generate_private_key() -> Result<String, TunnelError> {
    let mut bytes = [0u8; 32];
    aws_lc_rs::rand::fill(&mut bytes)
        .map_err(|_| TunnelError::BadKey("no randomness for a key".into()))?;
    let secret = StaticSecret::from(bytes);
    Ok(base64::engine::general_purpose::STANDARD.encode(secret.to_bytes()))
}

/// The public key for a base64 private key, like `wg pubkey` prints it.
pub fn public_key(private_key: &str) -> Result<String, TunnelError> {
    let secret = StaticSecret::from(decode_key(private_key)?);
    Ok(base64::engine::general_purpose::STANDARD.encode(PublicKey::from(&secret).as_bytes()))
}

/// Whether `key` is a WireGuard key: 32 bytes of base64.
pub fn public_key_is_valid(key: &str) -> bool {
    decode_key(key).is_ok()
}

fn decode_key(key: &str) -> Result<[u8; 32], TunnelError> {
    base64::engine::general_purpose::STANDARD
        .decode(key.trim())
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| TunnelError::BadKey("not 32 bytes of base64".into()))
}

/// A running tunnel. Dropping it closes every forwarded port and stops
/// talking to the machine.
pub struct Tunnel {
    runtime: Option<tokio::runtime::Runtime>,
    shared: Arc<Shared>,
    bound: Vec<(Forward, SocketAddr)>,
}

impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel")
            .field("endpoint", &self.shared.endpoint)
            .field("bound", &self.bound)
            .finish()
    }
}

impl Tunnel {
    /// Forwards [`moonlight_forwards`] to `peer`.
    pub fn open(peer: &WireGuardPeer) -> Result<Self, TunnelError> {
        Self::open_with(peer, &moonlight_forwards())
    }

    /// Forwards `forwards` to `peer`. Fails, naming the port, when a local
    /// port is taken. Returns before the WireGuard handshake; see
    /// [`Tunnel::wait_until_up`].
    pub fn open_with(peer: &WireGuardPeer, forwards: &[Forward]) -> Result<Self, TunnelError> {
        let private = StaticSecret::from(decode_key(&peer.client_private_key)?);
        let server = PublicKey::from(decode_key(&peer.server_public_key)?);
        let mut seed = [0u8; 12];
        aws_lc_rs::rand::fill(&mut seed).map_err(|_| TunnelError::Start("no randomness".into()))?;
        let index = u32::from_le_bytes([seed[0], seed[1], seed[2], 0]);
        let tunn = Tunn::new(private, server, None, Some(KEEPALIVE_SECONDS), index, None);

        // Bind everything first, with std sockets, so a taken port fails
        // here with its number, before anything runs.
        let mut tcp = Vec::new();
        let mut udp = Vec::new();
        let mut bound = Vec::new();
        for forward in forwards {
            let port = forward.local.port();
            let in_use = |e: io::Error| match e.kind() {
                io::ErrorKind::AddrInUse => TunnelError::PortInUse {
                    protocol: forward.protocol,
                    port,
                },
                _ => TunnelError::Bind {
                    protocol: forward.protocol,
                    port,
                    reason: e.to_string(),
                },
            };
            let addr = match forward.protocol {
                Protocol::Tcp => {
                    let listener = std::net::TcpListener::bind(forward.local).map_err(in_use)?;
                    listener.set_nonblocking(true).map_err(in_use)?;
                    let addr = listener.local_addr().map_err(in_use)?;
                    tcp.push((listener, forward.remote_port));
                    addr
                }
                Protocol::Udp => {
                    let socket = udp_socket(forward.local).map_err(in_use)?;
                    let addr = socket.local_addr().map_err(in_use)?;
                    udp.push((socket, forward.remote_port));
                    addr
                }
            };
            bound.push((*forward, addr));
        }
        let unspecified: IpAddr = match peer.endpoint {
            SocketAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
            SocketAddr::V6(_) => std::net::Ipv6Addr::UNSPECIFIED.into(),
        };
        let outside = udp_socket(SocketAddr::new(unspecified, 0))
            .map_err(|e| TunnelError::Start(e.to_string()))?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rega-tunnel")
            .enable_io()
            .enable_time()
            .build()
            .map_err(|e| TunnelError::Start(e.to_string()))?;
        let guard = runtime.enter();
        let to_tokio = |e: io::Error| TunnelError::Start(e.to_string());
        let mut udp_sockets = HashMap::new();
        for (socket, remote_port) in udp {
            let socket = Arc::new(UdpSocket::from_std(socket).map_err(to_tokio)?);
            udp_sockets.insert(remote_port, socket);
        }
        let tcp = tcp
            .into_iter()
            .map(|(l, port)| Ok((TcpListener::from_std(l)?, port)))
            .collect::<io::Result<Vec<_>>>()
            .map_err(to_tokio)?;
        let shared = Arc::new(Shared {
            wg: Mutex::new(tunn),
            outside: UdpSocket::from_std(outside).map_err(to_tokio)?,
            endpoint: peer.endpoint,
            client: peer.client_address,
            server: peer.server_address,
            mtu: usize::from(peer.mtu),
            ip_id: AtomicU16::new(u16::from_le_bytes([seed[3], seed[4]])),
            udp_peers: Mutex::new(HashMap::new()),
            udp_sockets,
            tcp_inbox: Mutex::new(VecDeque::new()),
            tcp_wake: Notify::new(),
        });
        for (remote_port, socket) in &shared.udp_sockets {
            runtime.spawn(udp_outbound(shared.clone(), socket.clone(), *remote_port));
        }
        let (accepted_tx, accepted_rx) = mpsc::channel(16);
        for (listener, remote_port) in tcp {
            runtime.spawn(accept(listener, remote_port, accepted_tx.clone()));
        }
        drop(accepted_tx);
        let tcp_seed = u64::from_le_bytes([
            seed[5], seed[6], seed[7], seed[8], seed[9], seed[10], seed[11], 0,
        ]);
        runtime.spawn(tcp_stack(shared.clone(), accepted_rx, tcp_seed));
        runtime.spawn(inbound(shared.clone()));
        runtime.spawn(timers(shared.clone()));
        drop(guard);
        Ok(Self {
            runtime: Some(runtime),
            shared,
            bound,
        })
    }

    /// The address a forward was bound to on this device, such as the free
    /// port picked for the Sunshine API.
    pub fn local_addr(&self, protocol: Protocol, remote_port: u16) -> Option<SocketAddr> {
        self.bound
            .iter()
            .find(|(f, _)| f.protocol == protocol && f.remote_port == remote_port)
            .map(|(_, addr)| *addr)
    }

    /// Time since the last WireGuard handshake with the machine; `None`
    /// until the first one.
    pub fn last_handshake(&self) -> Option<Duration> {
        self.shared.wg.lock().unwrap().stats().0
    }

    /// Waits for the first handshake with the machine.
    pub async fn wait_until_up(&self, timeout: Duration) -> Result<(), TunnelError> {
        let deadline = Instant::now() + timeout;
        while self.last_handshake().is_none() {
            if Instant::now() >= deadline {
                return Err(TunnelError::NoHandshake(timeout.as_secs()));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    /// Stops the tunnel and waits until its local ports are free again, so
    /// a new tunnel can take them right away. Dropping it stops it too, but
    /// without waiting.
    pub async fn close(mut self) {
        if let Some(runtime) = self.runtime.take() {
            let stop = move || runtime.shutdown_timeout(Duration::from_secs(2));
            let _ = tokio::task::spawn_blocking(stop).await;
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            // Dropping a runtime blocks, which isn't allowed inside another
            // runtime (the app's); this doesn't wait.
            runtime.shutdown_background();
        }
    }
}

struct Shared {
    wg: Mutex<Tunn>,
    /// Talks to the machine's WireGuard port. Unconnected, so a change of
    /// network on this device just picks the new route.
    outside: UdpSocket,
    endpoint: SocketAddr,
    client: Ipv4Addr,
    server: Ipv4Addr,
    mtu: usize,
    ip_id: AtomicU16,
    /// Who sent from each virtual UDP port: a local socket's port is used
    /// as its port inside the tunnel, so replies find their way back.
    udp_peers: Mutex<HashMap<u16, SocketAddr>>,
    /// The local UDP socket for each remote port; replies go out from it.
    udp_sockets: HashMap<u16, Arc<UdpSocket>>,
    /// IP packets for smoltcp.
    tcp_inbox: Mutex<VecDeque<Vec<u8>>>,
    tcp_wake: Notify,
}

impl Shared {
    /// Encrypts an IP packet for the machine. Without a session yet,
    /// boringtun holds the packet and returns a handshake to send instead.
    fn seal(&self, packet: &[u8]) -> Option<Vec<u8>> {
        let mut out = vec![0u8; (packet.len() + 32).max(148)];
        match self.wg.lock().unwrap().encapsulate(packet, &mut out) {
            TunnResult::WriteToNetwork(datagram) => Some(datagram.to_vec()),
            _ => None,
        }
    }

    /// For smoltcp, which can't wait: a full socket buffer drops the
    /// packet, as a busy network would, and TCP sends it again.
    fn send_ip(&self, packet: &[u8]) {
        if let Some(datagram) = self.seal(packet) {
            self.send_outside(&datagram);
        }
    }

    fn send_outside(&self, datagram: &[u8]) {
        let _ = self.outside.try_send_to(datagram, self.endpoint);
    }

    /// Waits for room in the socket buffer rather than dropping.
    async fn send_udp(&self, local_port: u16, remote_port: u16, payload: &[u8]) {
        let id = self.ip_id.fetch_add(1, Ordering::Relaxed);
        let src = SocketAddrV4::new(self.client, local_port);
        let dst = SocketAddrV4::new(self.server, remote_port);
        for packet in ipv4::udp_packets(src, dst, payload, id, self.mtu).unwrap_or_default() {
            if let Some(datagram) = self.seal(&packet) {
                let _ = self.outside.send_to(&datagram, self.endpoint).await;
            }
        }
    }

    /// Hands a packet from the machine to whoever waits for it.
    fn deliver(&self, packet: &[u8], joiner: &mut Reassembler) {
        let Some(header) = Header::parse(packet) else {
            return;
        };
        if header.src != self.server || header.dst != self.client {
            return;
        }
        let packet = if header.is_fragment() {
            match joiner.push(&header, packet, Instant::now()) {
                Some(whole) => Cow::Owned(whole),
                None => return,
            }
        } else {
            Cow::Borrowed(&packet[..header.total_len])
        };
        match header.protocol {
            ipv4::PROTO_UDP => {
                let Some((from, to, payload)) = ipv4::udp_payload(&packet) else {
                    return;
                };
                let peer = self.udp_peers.lock().unwrap().get(&to).copied();
                if let (Some(peer), Some(socket)) = (peer, self.udp_sockets.get(&from)) {
                    // Moonlight not keeping up drops the datagram; video
                    // has its own recovery.
                    let _ = socket.try_send_to(payload, peer);
                }
            }
            ipv4::PROTO_TCP => {
                self.tcp_inbox
                    .lock()
                    .unwrap()
                    .push_back(packet.into_owned());
                self.tcp_wake.notify_one();
            }
            _ => {}
        }
    }
}

/// A UDP socket with room for a burst of video, up to what the system
/// allows.
fn udp_socket(bind: SocketAddr) -> io::Result<std::net::UdpSocket> {
    let domain = socket2::Domain::for_address(bind);
    let socket = socket2::Socket::new(domain, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))?;
    let _ = socket.set_recv_buffer_size(UDP_BUFFER);
    let _ = socket.set_send_buffer_size(UDP_BUFFER);
    socket.set_nonblocking(true)?;
    socket.bind(&bind.into())?;
    Ok(socket.into())
}

/// Reads what the machine sends, decrypts it and delivers it.
async fn inbound(shared: Arc<Shared>) {
    let mut datagram = vec![0u8; MAX_DATAGRAM];
    let mut out = vec![0u8; MAX_DATAGRAM];
    let mut joiner = Reassembler::new();
    loop {
        let (len, from) = match shared.outside.recv_from(&mut datagram).await {
            Ok(received) => received,
            Err(_) => {
                // Such as an ICMP error from an earlier send; keep going.
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };
        if from != shared.endpoint {
            continue;
        }
        let result =
            shared
                .wg
                .lock()
                .unwrap()
                .decapsulate(Some(from.ip()), &datagram[..len], &mut out);
        match result {
            TunnResult::WriteToNetwork(reply) => {
                shared.send_outside(reply);
                // A finished handshake releases the packets boringtun held.
                loop {
                    let queued = shared.wg.lock().unwrap().decapsulate(None, &[], &mut out);
                    match queued {
                        TunnResult::WriteToNetwork(packet) => shared.send_outside(packet),
                        _ => break,
                    }
                }
            }
            TunnResult::WriteToTunnelV4(packet, _) => shared.deliver(packet, &mut joiner),
            _ => {}
        }
    }
}

/// Runs boringtun's timers: handshakes, their retries and the keepalive.
async fn timers(shared: Arc<Shared>) {
    let mut out = vec![0u8; 256];
    // Start the handshake now rather than with the first packet, so the
    // tunnel is up by the time Moonlight connects.
    let first = shared
        .wg
        .lock()
        .unwrap()
        .format_handshake_initiation(&mut out, false);
    if let TunnResult::WriteToNetwork(packet) = first {
        shared.send_outside(packet);
    }
    let mut tick = tokio::time::interval(TIMER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let mut wg = shared.wg.lock().unwrap();
        let result = match wg.update_timers(&mut out) {
            // Gave up on the last handshake; keep trying, the machine may
            // just be busy booting or this device between networks.
            TunnResult::Err(WireGuardError::ConnectionExpired) => {
                wg.format_handshake_initiation(&mut out, false)
            }
            other => other,
        };
        drop(wg);
        if let TunnResult::WriteToNetwork(packet) = result {
            shared.send_outside(packet);
        }
    }
}

/// Sends what a local UDP socket receives through the tunnel, from the
/// sender's own port number.
async fn udp_outbound(shared: Arc<Shared>, socket: Arc<UdpSocket>, remote_port: u16) {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((len, peer)) => {
                shared.udp_peers.lock().unwrap().insert(peer.port(), peer);
                shared.send_udp(peer.port(), remote_port, &buf[..len]).await;
            }
            // Such as a reply we sent after Moonlight closed its socket.
            Err(_) => tokio::time::sleep(Duration::from_millis(1)).await,
        }
    }
}

async fn accept(listener: TcpListener, remote_port: u16, accepted: mpsc::Sender<(TcpStream, u16)>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let _ = stream.set_nodelay(true);
                if accepted.send((stream, remote_port)).await.is_err() {
                    return;
                }
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

/// smoltcp's view of the tunnel: packets in from [`Shared::tcp_inbox`],
/// packets out straight into WireGuard.
struct TunnelDevice {
    shared: Arc<Shared>,
}

struct RxToken(Vec<u8>);
struct TxToken<'a>(&'a Shared);

impl smoltcp::phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl smoltcp::phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut packet = vec![0u8; len];
        let result = f(&mut packet);
        self.0.send_ip(&packet);
        result
    }
}

impl Device for TunnelDevice {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _: smoltcp::time::Instant) -> Option<(RxToken, TxToken<'_>)> {
        let packet = self.shared.tcp_inbox.lock().unwrap().pop_front()?;
        Some((RxToken(packet), TxToken(&self.shared)))
    }

    fn transmit(&mut self, _: smoltcp::time::Instant) -> Option<TxToken<'_>> {
        Some(TxToken(&self.shared))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = self.shared.mtu;
        // WireGuard already authenticates every packet.
        let mut checksum = ChecksumCapabilities::default();
        checksum.ipv4 = Checksum::Tx;
        checksum.tcp = Checksum::Tx;
        caps.checksum = checksum;
        caps
    }
}

/// One local TCP connection and its smoltcp socket.
struct Connection {
    handle: SocketHandle,
    local_port: u16,
    /// What the local side sent, waiting to go into the socket.
    from_local: mpsc::Receiver<Vec<u8>>,
    pending: Option<(Vec<u8>, usize)>,
    local_done: bool,
    /// What the machine sent, for the local side. `None` once the machine
    /// has finished sending.
    to_local: Option<mpsc::Sender<Vec<u8>>>,
}

impl Connection {
    /// Moves what it can each way. Returns whether anything moved.
    fn pump(&mut self, sockets: &mut SocketSet<'_>) -> bool {
        let socket = sockets.get_mut::<tcp::Socket>(self.handle);
        let mut moved = false;
        while !self.local_done {
            if !socket.may_send() {
                // Still connecting: the data waits. Otherwise the machine
                // has closed or reset the connection.
                if !matches!(
                    socket.state(),
                    tcp::State::SynSent | tcp::State::SynReceived
                ) {
                    self.local_done = true;
                }
                break;
            }
            if self.pending.is_none() {
                match self.from_local.try_recv() {
                    Ok(chunk) => self.pending = Some((chunk, 0)),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        self.local_done = true;
                        socket.close();
                        moved = true;
                        break;
                    }
                }
            }
            let Some((chunk, sent)) = self.pending.as_mut() else {
                break;
            };
            match socket.send_slice(&chunk[*sent..]) {
                Ok(0) => break,
                Ok(n) => {
                    moved = true;
                    *sent += n;
                    if *sent == chunk.len() {
                        self.pending = None;
                    }
                }
                Err(_) => {
                    // The connection is gone; nothing more goes out.
                    self.local_done = true;
                    break;
                }
            }
        }
        if let Some(to_local) = &self.to_local {
            while socket.can_recv() {
                match to_local.try_reserve() {
                    Ok(permit) => {
                        if let Ok(data) = socket.recv(|buf| (buf.len(), buf.to_vec())) {
                            permit.send(data);
                            moved = true;
                        }
                    }
                    // The local side is slow; smoltcp's window closes until
                    // it catches up.
                    Err(mpsc::error::TrySendError::Full(())) => break,
                    Err(mpsc::error::TrySendError::Closed(())) => {
                        socket.abort();
                        self.local_done = true;
                        break;
                    }
                }
            }
            if !socket.may_recv() && !socket.can_recv() && socket.state() != tcp::State::SynSent {
                self.to_local = None;
                moved = true;
            }
        }
        moved
    }

    fn finished(&self, sockets: &SocketSet<'_>) -> bool {
        let socket = sockets.get::<tcp::Socket>(self.handle);
        self.to_local.is_none()
            && matches!(socket.state(), tcp::State::Closed | tcp::State::TimeWait)
    }
}

async fn tcp_stack(shared: Arc<Shared>, mut accepted: mpsc::Receiver<(TcpStream, u16)>, seed: u64) {
    let mut device = TunnelDevice {
        shared: shared.clone(),
    };
    let mut config = Config::new(HardwareAddress::Ip);
    config.random_seed = seed;
    let mut iface = Interface::new(config, &mut device, smoltcp::time::Instant::now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(shared.client), 32));
    });
    let _ = iface.routes_mut().add_default_ipv4_route(shared.server);
    let mut sockets = SocketSet::new(Vec::new());
    let mut connections: Vec<Connection> = Vec::new();
    let mut next_port = 49152 + (seed % 16384) as u16;
    let wake = Arc::new(Notify::new());
    // False once no listener is left (a tunnel without TCP forwards).
    let mut accepting = true;

    loop {
        let now = smoltcp::time::Instant::now();
        iface.poll(now, &mut device, &mut sockets);
        let mut moved = false;
        for connection in &mut connections {
            moved |= connection.pump(&mut sockets);
        }
        if moved {
            iface.poll(now, &mut device, &mut sockets);
        }
        connections.retain(|c| {
            let done = c.finished(&sockets);
            if done {
                sockets.remove(c.handle);
            }
            !done
        });
        let delay = match iface.poll_delay(smoltcp::time::Instant::now(), &sockets) {
            Some(d) => Duration::from_micros(d.total_micros()),
            None => Duration::from_secs(1),
        };
        tokio::select! {
            _ = shared.tcp_wake.notified() => {}
            _ = wake.notified() => {}
            _ = tokio::time::sleep(delay) => {}
            new = accepted.recv(), if accepting => {
                let Some((stream, remote_port)) = new else {
                    accepting = false;
                    continue;
                };
                let used: Vec<u16> = connections.iter().map(|c| c.local_port).collect();
                while used.contains(&next_port) {
                    next_port = if next_port == u16::MAX { 49152 } else { next_port + 1 };
                }
                let local_port = next_port;
                next_port = if next_port == u16::MAX { 49152 } else { next_port + 1 };
                let mut socket = tcp::Socket::new(
                    tcp::SocketBuffer::new(vec![0; TCP_BUFFER]),
                    tcp::SocketBuffer::new(vec![0; TCP_BUFFER]),
                );
                socket.set_nagle_enabled(false);
                let remote = (IpAddress::Ipv4(shared.server), remote_port);
                if socket.connect(iface.context(), remote, local_port).is_err() {
                    continue;
                }
                let handle = sockets.add(socket);
                let (read, write) = stream.into_split();
                let (from_local_tx, from_local) = mpsc::channel(8);
                let (to_local, to_local_rx) = mpsc::channel(8);
                tokio::spawn(read_local(read, from_local_tx, wake.clone()));
                tokio::spawn(write_local(write, to_local_rx, wake.clone()));
                connections.push(Connection {
                    handle,
                    local_port,
                    from_local,
                    pending: None,
                    local_done: false,
                    to_local: Some(to_local),
                });
            }
        }
    }
}

async fn read_local(
    mut read: tokio::net::tcp::OwnedReadHalf,
    to_stack: mpsc::Sender<Vec<u8>>,
    wake: Arc<Notify>,
) {
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match read.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to_stack.send(buf[..n].to_vec()).await.is_err() {
                    break;
                }
                wake.notify_one();
            }
        }
    }
    drop(to_stack);
    wake.notify_one();
}

async fn write_local(
    mut write: tokio::net::tcp::OwnedWriteHalf,
    mut from_stack: mpsc::Receiver<Vec<u8>>,
    wake: Arc<Notify>,
) {
    while let Some(data) = from_stack.recv().await {
        if write.write_all(&data).await.is_err() {
            break;
        }
        // Room in the channel again.
        wake.notify_one();
    }
    let _ = write.shutdown().await;
    drop(from_stack);
    wake.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_like_wg() {
        let private = generate_private_key().unwrap();
        assert_eq!(private.len(), 44);
        assert_ne!(private, generate_private_key().unwrap());
        // `wg pubkey` of a known key.
        assert_eq!(
            public_key("YJ7D0kLqiOz7f7ae4YdxE9BmQwPFkn6p9VHZq4u0DFA=").unwrap(),
            "v++jn5TbyumgcnDNP7I1lcyU/BnB04Gsu/qedNNanzA="
        );
        assert!(public_key("short").is_err());
    }

    /// A WireGuard "machine" made of boringtun on 127.0.0.1 that echoes UDP
    /// back from the port it came to, fragmenting replies bigger than the
    /// MTU as Windows does. Returns its peer config for the client.
    fn fake_machine() -> (WireGuardPeer, std::thread::JoinHandle<()>) {
        let server_private = generate_private_key().unwrap();
        let client_private = generate_private_key().unwrap();
        let client_public =
            PublicKey::from(&StaticSecret::from(decode_key(&client_private).unwrap()));
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let peer = WireGuardPeer {
            endpoint: socket.local_addr().unwrap(),
            server_public_key: public_key(&server_private).unwrap(),
            client_private_key: client_private,
            client_address: Ipv4Addr::new(10, 213, 37, 2),
            server_address: Ipv4Addr::new(10, 213, 37, 1),
            mtu: 1400,
        };
        let server = StaticSecret::from(decode_key(&server_private).unwrap());
        let thread = std::thread::spawn(move || {
            let mut tunn = Tunn::new(server, client_public, None, None, 1, None);
            let mut joiner = Reassembler::new();
            let mut buf = vec![0u8; MAX_DATAGRAM];
            let mut out = vec![0u8; MAX_DATAGRAM];
            let mut id = 0u16;
            // Ends when the client has been quiet for the read timeout.
            while let Ok((len, from)) = socket.recv_from(&mut buf) {
                match tunn.decapsulate(Some(from.ip()), &buf[..len], &mut out) {
                    TunnResult::WriteToNetwork(reply) => {
                        socket.send_to(reply, from).unwrap();
                        while let TunnResult::WriteToNetwork(p) =
                            tunn.decapsulate(None, &[], &mut out)
                        {
                            socket.send_to(p, from).unwrap();
                        }
                    }
                    TunnResult::WriteToTunnelV4(packet, _) => {
                        let header = Header::parse(packet).unwrap();
                        let packet = if header.is_fragment() {
                            match joiner.push(&header, packet, Instant::now()) {
                                Some(whole) => whole,
                                None => continue,
                            }
                        } else {
                            packet.to_vec()
                        };
                        let Some((sport, dport, payload)) = ipv4::udp_payload(&packet) else {
                            continue;
                        };
                        id = id.wrapping_add(1);
                        let src = SocketAddrV4::new(header.dst, dport);
                        let dst = SocketAddrV4::new(header.src, sport);
                        for reply in ipv4::udp_packets(src, dst, payload, id, 1400).unwrap() {
                            let mut sealed = vec![0u8; reply.len() + 32];
                            if let TunnResult::WriteToNetwork(p) =
                                tunn.encapsulate(&reply, &mut sealed)
                            {
                                socket.send_to(p, from).unwrap();
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
        (peer, thread)
    }

    async fn echo(socket: &UdpSocket, payload: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        for _ in 0..3 {
            socket.send(payload).await.unwrap();
            let read = tokio::time::timeout(Duration::from_secs(2), socket.recv(&mut buf));
            if let Ok(Ok(n)) = read.await {
                return buf[..n].to_vec();
            }
        }
        panic!("no echo for {} bytes", payload.len());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn udp_round_trips_through_boringtun_including_fragments() {
        let (peer, machine) = fake_machine();
        let any = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let tunnel =
            Tunnel::open_with(&peer, &[Forward::udp(any, 47998), Forward::udp(any, 48000)])
                .unwrap();
        tunnel.wait_until_up(Duration::from_secs(10)).await.unwrap();
        assert!(tunnel.last_handshake().is_some());

        let video = UdpSocket::bind(any).await.unwrap();
        video
            .connect(tunnel.local_addr(Protocol::Udp, 47998).unwrap())
            .await
            .unwrap();
        let audio = UdpSocket::bind(any).await.unwrap();
        audio
            .connect(tunnel.local_addr(Protocol::Udp, 48000).unwrap())
            .await
            .unwrap();
        for len in [0, 1, 1372, 1373, 1392, 1400, 3000, 20000] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            assert_eq!(echo(&video, &payload).await, payload, "{len} bytes");
        }
        assert_eq!(echo(&audio, b"audio").await, b"audio");
        // Each reply came back once, from its own port: nothing else waits.
        let mut buf = [0u8; 16];
        let stray = tokio::time::timeout(Duration::from_millis(300), video.recv(&mut buf));
        assert!(stray.await.is_err(), "a reply arrived twice");

        tunnel.close().await;
        // Closing frees the ports for the next tunnel.
        let again = Tunnel::open_with(&peer, &[Forward::udp(any, 47998)]).unwrap();
        drop(again);
        drop(video);
        drop(audio);
        machine.join().unwrap();
    }

    #[test]
    fn a_taken_port_is_named() {
        let taken = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let peer = WireGuardPeer {
            endpoint: "127.0.0.1:9".parse().unwrap(),
            server_public_key: public_key(&generate_private_key().unwrap()).unwrap(),
            client_private_key: generate_private_key().unwrap(),
            client_address: Ipv4Addr::new(10, 213, 37, 2),
            server_address: Ipv4Addr::new(10, 213, 37, 1),
            mtu: 1400,
        };
        let err = Tunnel::open_with(&peer, &[Forward::udp(taken.local_addr().unwrap(), 47998)])
            .unwrap_err();
        assert_eq!(
            err,
            TunnelError::PortInUse {
                protocol: Protocol::Udp,
                port
            }
        );
        assert!(
            err.to_string().contains(&format!("UDP port {port} ")),
            "{err}"
        );

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let err = Tunnel::open_with(
            &peer,
            &[Forward::tcp(listener.local_addr().unwrap(), 47984)],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            TunnelError::PortInUse {
                protocol: Protocol::Tcp,
                ..
            }
        ));

        let bad = WireGuardPeer {
            client_private_key: "nope".into(),
            ..peer
        };
        assert!(matches!(
            Tunnel::open_with(&bad, &[]).unwrap_err(),
            TunnelError::BadKey(_)
        ));
        assert!(!format!("{bad:?}").contains("nope"));
    }

    #[tokio::test]
    async fn a_silent_machine_times_out_and_dropping_inside_a_runtime_is_fine() {
        // Nothing answers on the discard port.
        let peer = WireGuardPeer {
            endpoint: "127.0.0.1:9".parse().unwrap(),
            server_public_key: public_key(&generate_private_key().unwrap()).unwrap(),
            client_private_key: generate_private_key().unwrap(),
            client_address: Ipv4Addr::new(10, 213, 37, 2),
            server_address: Ipv4Addr::new(10, 213, 37, 1),
            mtu: 1400,
        };
        let tunnel = Tunnel::open_with(&peer, &moonlight_forwards()[8..]).unwrap();
        assert_eq!(
            tunnel.wait_until_up(Duration::from_millis(300)).await,
            Err(TunnelError::NoHandshake(0))
        );
        drop(tunnel);
    }

    #[test]
    fn moonlight_ports_and_a_free_api_port() {
        let forwards = moonlight_forwards();
        assert_eq!(forwards.len(), 9);
        assert!(forwards.iter().all(|f| f.local.ip() == Ipv4Addr::LOCALHOST));
        let api = forwards
            .iter()
            .find(|f| f.remote_port == SUNSHINE_API_PORT)
            .unwrap();
        assert_eq!((api.protocol, api.local.port()), (Protocol::Tcp, 0));
        for port in MOONLIGHT_TCP_PORTS {
            assert!(forwards.contains(&Forward::tcp(([127, 0, 0, 1], port).into(), port)));
        }
        for port in MOONLIGHT_UDP_PORTS {
            assert!(forwards.contains(&Forward::udp(([127, 0, 0, 1], port).into(), port)));
        }
    }
}
