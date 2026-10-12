//! Streaming from a running machine: the WireGuard tunnel to it, pairing
//! Moonlight with its Sunshine, and starting Moonlight. Nothing here knows
//! about a cloud; a provider says where the machine is
//! ([`crate::CloudProvider::stream_target`]).
//!
//! Pairing is automatic when ReGa can run Moonlight itself: it picks a PIN,
//! starts `moonlight pair` with it and hands Sunshine the same PIN. Where
//! it can't, as inside the Flatpak, the user adds the host in Moonlight and
//! types the PIN Moonlight shows into ReGa, which hands it to Sunshine.

#[cfg(test)]
mod fake;
mod ipv4;
pub mod moonlight;
pub mod sunshine;
pub mod tunnel;

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use moonlight::Moonlight;
use sunshine::{SunshineApi, SunshineError};
use tunnel::{Protocol, Tunnel, TunnelError, SUNSHINE_API_PORT};

/// The Sunshine app Moonlight streams: the whole desktop, where Steam
/// starts at logon.
pub const STREAM_APP: &str = "Desktop";
/// How long the first WireGuard handshake may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// How long automatic pairing waits for each of Moonlight's steps.
const PAIR_TIMEOUT: Duration = Duration::from_secs(60);
/// How often it looks for Moonlight's pairing request.
const PAIR_POLL: Duration = Duration::from_millis(500);
/// How long a stream that fails at once takes to say so.
const STREAM_GRACE: Duration = Duration::from_secs(3);

/// The machine's address inside the tunnel (the playbook's
/// `rega_wireguard_server_address`).
pub const SERVER_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 213, 37, 1);
/// This device's address inside the tunnel (`rega_wireguard_client_address`).
pub const CLIENT_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 213, 37, 2);
/// The tunnel's MTU (`rega_wireguard_mtu`): 1400 fits paths down to 1460
/// bytes, such as PPPoE and DS-Lite.
pub const TUNNEL_MTU: u16 = 1400;
/// Sunshine's web UI and API login (`rega_sunshine_username`).
pub const SUNSHINE_USER: &str = "rega";

/// What a device needs to stream from a running machine.
#[derive(Clone, PartialEq, Eq)]
pub struct StreamTarget {
    pub machine_id: String,
    pub public_ip: String,
    pub sunshine_user: String,
    pub sunshine_password: String,
    /// Lowercase hex; `None` until a setup has read it.
    pub sunshine_cert_sha256: Option<String>,
    /// `None` when the machine has WireGuard off: Moonlight then goes to
    /// the public IP.
    pub wireguard: Option<WireGuardPeer>,
}

impl std::fmt::Debug for StreamTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamTarget")
            .field("machine_id", &self.machine_id)
            .field("public_ip", &self.public_ip)
            .field("sunshine_user", &self.sunshine_user)
            .field("sunshine_password", &"…")
            .field("sunshine_cert_sha256", &self.sunshine_cert_sha256)
            .field("wireguard", &self.wireguard)
            .finish()
    }
}

/// The machine's end of the WireGuard tunnel, and this device's key for it.
#[derive(Clone, PartialEq, Eq)]
pub struct WireGuardPeer {
    /// The machine's public IP and WireGuard port.
    pub endpoint: SocketAddr,
    pub server_public_key: String,
    pub client_private_key: String,
    /// This device's address inside the tunnel, 10.213.37.2.
    pub client_address: Ipv4Addr,
    /// The machine's address inside the tunnel, 10.213.37.1.
    pub server_address: Ipv4Addr,
    pub mtu: u16,
}

impl std::fmt::Debug for WireGuardPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireGuardPeer")
            .field("endpoint", &self.endpoint)
            .field("server_public_key", &self.server_public_key)
            .field("client_private_key", &"…")
            .field("client_address", &self.client_address)
            .field("server_address", &self.server_address)
            .field("mtu", &self.mtu)
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum StreamError {
    #[error("{0}")]
    Tunnel(#[from] TunnelError),
    #[error("{0}")]
    Sunshine(#[from] SunshineError),
    #[error(
        "Sunshine didn't accept that PIN. Pair again in Moonlight and type the new PIN it shows"
    )]
    WrongPin,
    #[error("the PIN is the 4 digits Moonlight shows")]
    BadPin,
    #[error(
        "Sunshine has no pairing request from Moonlight. In Moonlight, add the host {0} \
         (or pick it) and choose to pair, then type the PIN it shows"
    )]
    NoRequest(String),
    #[error("Moonlight: {0}")]
    Moonlight(String),
    #[error("no connection to that machine; press Play first")]
    NotConnected,
    #[error("{0}")]
    Other(String),
}

pub type StreamResult<T> = Result<T, StreamError>;

/// The name this device pairs under: `ReGa on <hostname>`.
pub fn device_name() -> String {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty());
    match host {
        Some(host) => format!("ReGa on {host}"),
        None => "ReGa".into(),
    }
}

/// A connection to one machine's Sunshine: through the WireGuard tunnel when
/// the machine has it on, else straight to its public IP.
pub struct Connection {
    target: StreamTarget,
    /// What Moonlight connects to: 127.0.0.1 through the tunnel, else the
    /// machine's public IP.
    host: String,
    /// Where Sunshine sees this device's pairing requests come from, when
    /// known: this device's tunnel address.
    pairing_from: Option<String>,
    api: SunshineApi,
    tunnel: Option<Tunnel>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("machine_id", &self.target.machine_id)
            .field("host", &self.host)
            .field("tunnel", &self.tunnel)
            .finish()
    }
}

impl Connection {
    /// Brings the tunnel up (WireGuard on) and waits for the machine to
    /// answer through it.
    pub async fn open(target: StreamTarget) -> StreamResult<Self> {
        Self::open_at(target, SUNSHINE_API_PORT).await
    }

    async fn open_at(target: StreamTarget, api_port: u16) -> StreamResult<Self> {
        let user = &target.sunshine_user;
        let password = &target.sunshine_password;
        let cert = target.sunshine_cert_sha256.as_deref();
        if let Some(peer) = &target.wireguard {
            let tunnel = Tunnel::open(peer)?;
            tunnel.wait_until_up(HANDSHAKE_TIMEOUT).await?;
            let api_addr = tunnel
                .local_addr(Protocol::Tcp, SUNSHINE_API_PORT)
                .ok_or_else(|| StreamError::Other("the tunnel has no API port".into()))?;
            // The tunnel already proves it's the machine; the certificate
            // is checked too when the setup has seen it.
            let api = SunshineApi::new(api_addr, user, password, cert)?;
            return Ok(Self {
                host: Ipv4Addr::LOCALHOST.to_string(),
                pairing_from: Some(peer.client_address.to_string()),
                api,
                tunnel: Some(tunnel),
                target,
            });
        }
        // Over the internet the password goes only to the certificate the
        // setup saw.
        let Some(cert) = cert else {
            return Err(StreamError::Other(
                "Sunshine's certificate isn't known yet; start the machine again to set it up"
                    .into(),
            ));
        };
        let ip: IpAddr = target
            .public_ip
            .parse()
            .map_err(|_| StreamError::Other(format!("odd public IP {}", target.public_ip)))?;
        let api = SunshineApi::new((ip, api_port).into(), user, password, Some(cert))?;
        Ok(Self {
            host: target.public_ip.clone(),
            pairing_from: None,
            api,
            tunnel: None,
            target,
        })
    }

    pub fn machine_id(&self) -> &str {
        &self.target.machine_id
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn through_wireguard(&self) -> bool {
        self.tunnel.is_some()
    }

    /// Stops the tunnel, if any, and waits until its ports are free.
    pub async fn close(self) {
        if let Some(tunnel) = self.tunnel {
            tunnel.close().await;
        }
    }

    /// Pairs Moonlight with Sunshine with no PIN typing: Moonlight asks with
    /// a PIN ReGa picked, and ReGa hands Sunshine the same one.
    pub async fn pair_automatically(&self, moonlight: &Moonlight, name: &str) -> StreamResult<()> {
        self.pair_automatically_within(moonlight, name, PAIR_TIMEOUT)
            .await
    }

    async fn pair_automatically_within(
        &self,
        moonlight: &Moonlight,
        name: &str,
        timeout: Duration,
    ) -> StreamResult<()> {
        let pin = sunshine::new_pin()?;
        // Requests already waiting aren't the one Moonlight is about to make.
        let before: HashSet<String> = self
            .api
            .pending()
            .await?
            .into_iter()
            .map(|p| p.id)
            .collect();
        let mut pair = moonlight
            .pair(&self.host, &pin)
            .await
            .map_err(|e| StreamError::Moonlight(format!("couldn't start it: {e}")))?;
        let deadline = Instant::now() + timeout;
        let request = loop {
            if let Ok(Some(status)) = pair.try_wait() {
                return Err(StreamError::Moonlight(format!(
                    "it stopped before asking to pair ({status})"
                )));
            }
            let pending = self.api.pending().await?;
            if let Some(request) = pending
                .into_iter()
                .rev()
                .find(|p| !before.contains(&p.id) && self.asked_from_here(&p.address))
            {
                break request;
            }
            if Instant::now() >= deadline {
                return Err(StreamError::Moonlight(
                    "it didn't ask Sunshine to pair in time".into(),
                ));
            }
            tokio::time::sleep(PAIR_POLL).await;
        };
        // Answers once Moonlight has finished its side, or failed.
        match self.api.submit_pin(&request.id, &pin, name).await {
            Ok(true) => {}
            Ok(false) => return Err(StreamError::Moonlight("pairing failed; try again".into())),
            Err(e) => {
                let _ = self.api.cancel(&request.id).await;
                return Err(e.into());
            }
        }
        match tokio::time::timeout(timeout, pair.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(status)) => Err(StreamError::Moonlight(format!(
                "pairing ended with {status}"
            ))),
            _ => Err(StreamError::Moonlight("pairing didn't finish".into())),
        }
    }

    /// Hands the PIN Moonlight shows to its pairing request, the newest one
    /// from this device.
    pub async fn pair_with_pin(&self, pin: &str, name: &str) -> StreamResult<()> {
        let pin = pin.trim();
        if pin.len() != 4 || !pin.bytes().all(|b| b.is_ascii_digit()) {
            return Err(StreamError::BadPin);
        }
        let request = self
            .api
            .pending()
            .await?
            .into_iter()
            .rev()
            .find(|p| self.asked_from_here(&p.address))
            .ok_or_else(|| StreamError::NoRequest(self.host.clone()))?;
        if self.api.submit_pin(&request.id, pin, name).await? {
            Ok(())
        } else {
            Err(StreamError::WrongPin)
        }
    }

    /// Whether Sunshine has a client paired under `name`. Only a hint:
    /// Moonlight may have lost its side since.
    pub async fn paired_as(&self, name: &str) -> StreamResult<bool> {
        Ok(self.api.clients().await?.iter().any(|c| c.name == name))
    }

    fn asked_from_here(&self, address: &str) -> bool {
        self.pairing_from.as_deref().is_none_or(|a| a == address)
    }
}

/// A step of Play, for the UI to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayStep {
    Connecting,
    Pairing,
    StartingMoonlight,
}

/// How Play ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayOutcome {
    /// Moonlight is streaming the machine's desktop.
    Streaming,
    /// ReGa can't run Moonlight here (as inside the Flatpak): the user
    /// streams from Moonlight's own window, connecting to `host`. Unless
    /// `paired`, Moonlight pairs first and ReGa takes the PIN it shows.
    OpenMoonlight { host: String, paired: bool },
}

/// What is connected right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStatus {
    pub machine_id: String,
    pub host: String,
    /// The WireGuard tunnel is up; quitting ReGa ends the stream.
    pub wireguard: bool,
}

/// The connection Play keeps: one machine at a time, like the rest of ReGa.
#[derive(Default)]
pub struct Streams {
    current: tokio::sync::Mutex<Option<Connection>>,
    /// A copy of what `current` is, readable while Play holds it.
    status: std::sync::Mutex<Option<StreamStatus>>,
}

impl Streams {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connects to the machine (or keeps the connection Play made before),
    /// pairs Moonlight if it isn't, and starts the stream. Without a
    /// Moonlight to run, stops at connecting and says what to do in
    /// Moonlight instead.
    pub async fn play(
        &self,
        target: StreamTarget,
        moonlight: Option<&Moonlight>,
        progress: &(dyn Fn(PlayStep) + Send + Sync),
    ) -> StreamResult<PlayOutcome> {
        self.play_within(target, moonlight, progress, SUNSHINE_API_PORT, PAIR_TIMEOUT)
            .await
    }

    async fn play_within(
        &self,
        target: StreamTarget,
        moonlight: Option<&Moonlight>,
        progress: &(dyn Fn(PlayStep) + Send + Sync),
        api_port: u16,
        pair_timeout: Duration,
    ) -> StreamResult<PlayOutcome> {
        progress(PlayStep::Connecting);
        let mut current = self.current.lock().await;
        let opened = current.as_ref().is_none_or(|c| c.target != target);
        if opened {
            if let Some(old) = current.take() {
                self.set_status(None);
                old.close().await;
            }
            let connection = Connection::open_at(target, api_port).await?;
            self.set_status(Some(StreamStatus {
                machine_id: connection.machine_id().into(),
                host: connection.host().into(),
                wireguard: connection.through_wireguard(),
            }));
            *current = Some(connection);
        }
        let connection = current.as_ref().expect("connected above");
        let result = Self::use_connection(connection, moonlight, progress, pair_timeout).await;
        if result.is_err() && opened {
            // A connection this Play made and couldn't use doesn't stay up,
            // looking like a stream. One an earlier Play made may carry its
            // stream still.
            if let Some(connection) = current.take() {
                self.set_status(None);
                connection.close().await;
            }
        }
        result
    }

    /// Pairs Moonlight through `connection` if it isn't, and starts the
    /// stream.
    async fn use_connection(
        connection: &Connection,
        moonlight: Option<&Moonlight>,
        progress: &(dyn Fn(PlayStep) + Send + Sync),
        pair_timeout: Duration,
    ) -> StreamResult<PlayOutcome> {
        let name = device_name();
        let Some(moonlight) = moonlight else {
            let paired = connection.paired_as(&name).await.unwrap_or(false);
            return Ok(PlayOutcome::OpenMoonlight {
                host: connection.host().into(),
                paired,
            });
        };
        if !moonlight.is_paired(connection.host()).await {
            progress(PlayStep::Pairing);
            connection
                .pair_automatically_within(moonlight, &name, pair_timeout)
                .await?;
        }
        progress(PlayStep::StartingMoonlight);
        let mut stream = moonlight
            .stream(connection.host(), STREAM_APP)
            .await
            .map_err(|e| StreamError::Moonlight(format!("couldn't start it: {e}")))?;
        // A stream that can't start ends at once; one that runs carries on
        // after ReGa stops watching.
        match tokio::time::timeout(STREAM_GRACE, stream.wait()).await {
            Ok(Ok(status)) if !status.success() => Err(StreamError::Moonlight(format!(
                "the stream didn't start ({status})"
            ))),
            _ => Ok(PlayOutcome::Streaming),
        }
    }

    /// Hands Moonlight's PIN to the pairing request it made to the machine
    /// Play connected to.
    pub async fn pair_with_pin(&self, machine_id: &str, pin: &str) -> StreamResult<()> {
        let current = self.current.lock().await;
        let connection = current
            .as_ref()
            .filter(|c| c.machine_id() == machine_id)
            .ok_or(StreamError::NotConnected)?;
        connection.pair_with_pin(pin, &device_name()).await
    }

    pub fn status(&self) -> Option<StreamStatus> {
        self.status.lock().unwrap().clone()
    }

    /// Closes the connection, ending a stream through the tunnel.
    pub async fn close(&self) {
        let old = self.current.lock().await.take();
        self.set_status(None);
        if let Some(old) = old {
            old.close().await;
        }
    }

    /// Closes the connection if it is to `machine_id`, as before a stop.
    pub async fn close_machine(&self, machine_id: &str) {
        if self.status().is_some_and(|s| s.machine_id == machine_id) {
            self.close().await;
        }
    }

    fn set_status(&self, status: Option<StreamStatus>) {
        *self.status.lock().unwrap() = status;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::fake::{FakeSunshine, Request};
    use crate::stream::moonlight::tests::script;
    use serde_json::{json, Value};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    const OLD: &str = "00000000000000000000000000000000";
    const NEW: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn the_tunnel_matches_the_playbook() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../infra/ansible/roles");
        let wireguard = std::fs::read_to_string(root.join("wireguard/defaults/main.yml")).unwrap();
        for line in [
            format!("rega_wireguard_server_address: {SERVER_ADDRESS}"),
            format!("rega_wireguard_client_address: {CLIENT_ADDRESS}/32"),
            format!("rega_wireguard_mtu: {TUNNEL_MTU}"),
            format!("rega_wireguard_port: {}", crate::ansible::WIREGUARD_PORT),
        ] {
            assert!(wireguard.lines().any(|l| l == line), "{line}");
        }
        let sunshine = std::fs::read_to_string(root.join("sunshine/defaults/main.yml")).unwrap();
        let user = format!("rega_sunshine_username: {SUNSHINE_USER}");
        assert!(sunshine.lines().any(|l| l == user));
    }

    #[test]
    fn this_device_has_a_name() {
        let name = device_name();
        assert!(name.starts_with("ReGa"), "{name}");
        assert!(name.len() <= 128);
    }

    /// A target for the fake Sunshine on 127.0.0.1, as without WireGuard.
    fn target(sunshine: &FakeSunshine) -> StreamTarget {
        StreamTarget {
            machine_id: "m-1".into(),
            public_ip: "127.0.0.1".into(),
            sunshine_user: "rega".into(),
            sunshine_password: "pw".into(),
            sunshine_cert_sha256: Some(sunshine.cert_sha256()),
            wireguard: None,
        }
    }

    /// A connection as through the tunnel: Moonlight's requests come from
    /// 10.213.37.2.
    fn through_tunnel(sunshine: &FakeSunshine) -> Connection {
        Connection {
            target: target(sunshine),
            host: "127.0.0.1".into(),
            pairing_from: Some("10.213.37.2".into()),
            api: sunshine.api("rega", "pw", true),
            tunnel: None,
        }
    }

    fn pairing(id: &str, address: &str) -> Value {
        json!({"id": id, "name": "roth", "address": address})
    }

    /// A Sunshine and a Moonlight that pair the way the real ones do:
    /// `moonlight pair` makes a request with its PIN and waits; Sunshine
    /// completes it when given the same PIN, and fails it otherwise.
    struct Pairing {
        dir: tempfile::TempDir,
        sunshine: FakeSunshine,
        moonlight: Moonlight,
    }

    async fn pairing_pair(moonlight_script: &str) -> Pairing {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        let state = d.clone();
        let sunshine = FakeSunshine::start(move |req: &Request| {
            let pin_file = state.join("pin");
            match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/api/pin") => {
                    // Someone else's request, from before, and from
                    // elsewhere; Moonlight's once it has asked.
                    let mut pairings = vec![
                        pairing(OLD, "10.213.37.2"),
                        pairing(&"f".repeat(32), "192.0.2.5"),
                    ];
                    if pin_file.exists() && !state.join("done").exists() {
                        pairings.push(pairing(NEW, "10.213.37.2"));
                    }
                    (200, json!({ "pairings": pairings }))
                }
                ("POST", "/api/pin") => {
                    let body: Value = serde_json::from_slice(&req.body).unwrap();
                    let expected = std::fs::read_to_string(&pin_file).unwrap_or_default();
                    let ok = body["pairing_id"] == NEW && body["pin"] == expected.trim();
                    let outcome = if ok { "done" } else { "failed" };
                    std::fs::write(state.join(outcome), "").unwrap();
                    (200, json!({ "status": ok }))
                }
                ("DELETE", "/api/pin") => (200, json!({"status": true})),
                ("GET", "/api/clients/list") => {
                    let clients: Vec<Value> = if state.join("done").exists() {
                        vec![json!({"name": crate::stream::device_name(), "uuid": "u"})]
                    } else {
                        vec![]
                    };
                    (200, json!({"named_certs": clients}))
                }
                _ => (404, json!({})),
            }
        })
        .await;
        let fake = script(
            &d,
            "moonlight",
            &moonlight_script.replace("$DIR", &d.display().to_string()),
        );
        let moonlight = Moonlight::find_with(Some(fake.into_os_string()), None, None)
            .await
            .unwrap();
        Pairing {
            dir,
            sunshine,
            moonlight,
        }
    }

    /// Like moonlight-qt: `pair` asks with its PIN and exits 0 once
    /// paired, 1 if not; `list` works once paired; `stream` logs.
    const MOONLIGHT: &str = r#"echo "$@" >> $DIR/moonlight.log
case $1 in
  pair)
    echo "$4" > $DIR/pin
    for i in $(seq 300); do
      [ -e $DIR/done ] && exit 0
      [ -e $DIR/failed ] && exit 1
      sleep 0.1
    done
    exit 1 ;;
  list) [ -e $DIR/done ] ;;
  stream) exit 0 ;;
esac"#;

    fn log(p: &Pairing) -> String {
        std::fs::read_to_string(p.dir.path().join("moonlight.log")).unwrap_or_default()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn automatic_pairing_hands_sunshine_moonlights_pin() {
        let p = pairing_pair(MOONLIGHT).await;
        let connection = through_tunnel(&p.sunshine);
        connection
            .pair_automatically_within(&p.moonlight, "ReGa on desk", Duration::from_secs(20))
            .await
            .unwrap();
        let pin = std::fs::read_to_string(p.dir.path().join("pin")).unwrap();
        assert!(log(&p).starts_with(&format!("pair 127.0.0.1 --pin {}", pin.trim())));
        let posts: Vec<Value> = p
            .sunshine
            .requests()
            .iter()
            .filter(|r| r.method == "POST")
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        // Only Moonlight's new request, not the older one or another
        // device's, got the PIN.
        assert_eq!(
            posts,
            [json!({"pairing_id": NEW, "pin": pin.trim(), "name": "ReGa on desk"})]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn automatic_pairing_gives_up_cleanly() {
        // Moonlight never asks.
        let p = pairing_pair("exit 3").await;
        let connection = through_tunnel(&p.sunshine);
        let err = connection
            .pair_automatically_within(&p.moonlight, "x", Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("stopped before asking"), "{err}");

        // Moonlight hangs without asking: killed after the timeout.
        let p = pairing_pair("sleep 30").await;
        let connection = through_tunnel(&p.sunshine);
        let started = Instant::now();
        let err = connection
            .pair_automatically_within(&p.moonlight, "x", Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("in time"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_pin_submission_cancels_the_request() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        let state = d.clone();
        let sunshine = FakeSunshine::start(move |req: &Request| {
            match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/api/pin") if state.join("pin").exists() => {
                    (200, json!({"pairings": [pairing(NEW, "10.213.37.2")]}))
                }
                ("GET", "/api/pin") => (200, json!({"pairings": []})),
                ("POST", "/api/pin") => (400, json!({"status": false, "error": "broken"})),
                _ => (200, json!({"status": true})),
            }
        })
        .await;
        let fake = script(
            &d,
            "moonlight",
            &format!("echo $4 > {}/pin; sleep 30", d.display()),
        );
        let moonlight = Moonlight::find_with(Some(fake.into_os_string()), None, None)
            .await
            .unwrap();
        let err = through_tunnel(&sunshine)
            .pair_automatically_within(&moonlight, "x", Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            StreamError::Sunshine(SunshineError::Api("broken".into()))
        );
        let deletes: Vec<Value> = sunshine
            .requests()
            .iter()
            .filter(|r| r.method == "DELETE")
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        assert_eq!(deletes, [json!({"pairing_id": NEW})]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn manual_pairing_takes_the_newest_request_asked_from_here() {
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let log = seen.clone();
        let sunshine = FakeSunshine::start(move |req: &Request| match req.method.as_str() {
            "GET" => (
                200,
                json!({"pairings": [
                    pairing(OLD, "10.213.37.2"),
                    pairing(NEW, "10.213.37.2"),
                    pairing(&"f".repeat(32), "192.0.2.5"),
                ]}),
            ),
            _ => {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                log.lock().unwrap().push(body.clone());
                (200, json!({"status": body["pin"] == "4321"}))
            }
        })
        .await;
        let connection = through_tunnel(&sunshine);
        assert_eq!(
            connection.pair_with_pin("1234", "ReGa on desk").await,
            Err(StreamError::WrongPin)
        );
        connection
            .pair_with_pin(" 4321 ", "ReGa on desk")
            .await
            .unwrap();
        let bodies = seen.lock().unwrap().clone();
        assert!(bodies.iter().all(|b| b["pairing_id"] == NEW), "{bodies:?}");
        for bad in ["", "12", "12345", "abcd"] {
            assert_eq!(
                connection.pair_with_pin(bad, "x").await,
                Err(StreamError::BadPin)
            );
        }
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn manual_pairing_without_a_request_says_what_to_do() {
        let sunshine =
            FakeSunshine::start(|_| (200, json!({"pairings": [pairing(OLD, "192.0.2.5")]}))).await;
        let err = through_tunnel(&sunshine)
            .pair_with_pin("1234", "x")
            .await
            .unwrap_err();
        assert_eq!(err, StreamError::NoRequest("127.0.0.1".into()));
        assert!(err.to_string().contains("add the host 127.0.0.1"), "{err}");
        // Without the tunnel the address isn't this device's tunnel one,
        // so any request will do.
        let connection = Connection::open_at(target(&sunshine), sunshine.addr().port())
            .await
            .unwrap();
        assert!(connection.pair_with_pin("1234", "x").await.is_err());
        assert_eq!(connection.host(), "127.0.0.1");
        assert!(!connection.through_wireguard());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn play_pairs_once_then_streams() {
        let p = pairing_pair(MOONLIGHT).await;
        let streams = Streams::new();
        let steps = Arc::new(Mutex::new(Vec::new()));
        let record = {
            let steps = steps.clone();
            move |s: PlayStep| steps.lock().unwrap().push(s)
        };
        let port = p.sunshine.addr().port();
        let outcome = streams
            .play_within(
                target(&p.sunshine),
                Some(&p.moonlight),
                &record,
                port,
                Duration::from_secs(20),
            )
            .await
            .unwrap();
        assert_eq!(outcome, PlayOutcome::Streaming);
        assert_eq!(
            *steps.lock().unwrap(),
            [
                PlayStep::Connecting,
                PlayStep::Pairing,
                PlayStep::StartingMoonlight
            ]
        );
        assert_eq!(
            streams.status(),
            Some(StreamStatus {
                machine_id: "m-1".into(),
                host: "127.0.0.1".into(),
                wireguard: false,
            })
        );
        // Paired now: the next Play goes straight to the stream.
        steps.lock().unwrap().clear();
        streams
            .play_within(
                target(&p.sunshine),
                Some(&p.moonlight),
                &record,
                port,
                Duration::from_secs(20),
            )
            .await
            .unwrap();
        assert_eq!(
            *steps.lock().unwrap(),
            [PlayStep::Connecting, PlayStep::StartingMoonlight]
        );
        let log = log(&p);
        assert!(log.contains("stream 127.0.0.1 Desktop\n"), "{log}");
        assert_eq!(log.matches("pair ").count(), 1, "{log}");

        streams.close_machine("another").await;
        assert!(streams.status().is_some());
        streams.close_machine("m-1").await;
        assert_eq!(streams.status(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn play_without_moonlight_asks_for_the_pin() {
        let p = pairing_pair(MOONLIGHT).await;
        let streams = Streams::new();
        let port = p.sunshine.addr().port();
        let outcome = streams
            .play_within(
                target(&p.sunshine),
                None,
                &|_| {},
                port,
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            PlayOutcome::OpenMoonlight {
                host: "127.0.0.1".into(),
                paired: false
            }
        );
        // Not connected to that machine.
        assert_eq!(
            streams.pair_with_pin("other", "1234").await,
            Err(StreamError::NotConnected)
        );
        // Moonlight asks; the user types the PIN it shows.
        std::fs::write(p.dir.path().join("pin"), "2468").unwrap();
        streams.pair_with_pin("m-1", "2468").await.unwrap();
        let outcome = streams
            .play_within(
                target(&p.sunshine),
                None,
                &|_| {},
                port,
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            PlayOutcome::OpenMoonlight {
                host: "127.0.0.1".into(),
                paired: true
            }
        );
        streams.close().await;
        assert_eq!(streams.status(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stream_that_fails_at_once_says_so() {
        let p = pairing_pair(&MOONLIGHT.replace("stream) exit 0", "stream) exit 4")).await;
        std::fs::write(p.dir.path().join("done"), "").unwrap();
        let streams = Streams::new();
        let err = streams
            .play_within(
                target(&p.sunshine),
                Some(&p.moonlight),
                &|_| {},
                p.sunshine.addr().port(),
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("didn't start"), "{err}");
        // Nothing left connected that looks like a stream.
        assert_eq!(streams.status(), None);
        assert!(streams.current.lock().await.is_none());
    }

    #[tokio::test]
    async fn over_the_internet_the_certificate_is_required() {
        let sunshine = FakeSunshine::start(|_| (200, json!({}))).await;
        let mut t = target(&sunshine);
        t.sunshine_cert_sha256 = None;
        let err = Connection::open_at(t, sunshine.addr().port())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("certificate"), "{err}");
    }
}
