//! Checks a machine the playbook set up the way Play reaches it: through
//! ReGa's own WireGuard tunnel to Sunshine, with the generated login and
//! the certificate fingerprint the setup recorded. CI runs it in WSL on the
//! Windows runner after the playbook (`.github/workflows/ansible.yml`), so
//! the tunnel, the machine's firewall and Sunshine are tested together
//! without AWS.
//!
//! ```text
//! rega-stream-check keygen <key file>
//!     Writes a new WireGuard private key and prints its public key, for
//!     the playbook's rega_wireguard_client_public_key.
//! rega-stream-check check <secrets dir> <key file> [endpoint]
//!     Reads connection.json and sunshine-password from the playbook's
//!     secrets folder and checks the machine at endpoint (by default
//!     127.0.0.1:51820, the machine's WireGuard port).
//! ```
//!
//! It forwards free local ports rather than Moonlight's own, which a local
//! Sunshine (as on the runner) already has.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use rega_core::ansible::parse_connection;
use rega_core::stream::sunshine::{SunshineApi, SunshineError};
use rega_core::stream::tunnel::{self, Forward, Protocol, Tunnel, SUNSHINE_API_PORT};
use rega_core::stream::{WireGuardPeer, CLIENT_ADDRESS, SERVER_ADDRESS, SUNSHINE_USER, TUNNEL_MTU};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Sunshine's HTTP port, which Moonlight asks for the server's details.
const NVHTTP_PORT: u16 = 47989;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

type CheckResult<T = ()> = Result<T, String>;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        ["keygen", key_file] => keygen(Path::new(key_file)),
        ["check", secrets, key_file] => check(Path::new(secrets), Path::new(key_file), None).await,
        ["check", secrets, key_file, endpoint] => {
            check(Path::new(secrets), Path::new(key_file), Some(endpoint)).await
        }
        _ => Err("usage: rega-stream-check keygen <key file>\n       \
             rega-stream-check check <secrets dir> <key file> [endpoint]"
            .into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("FAILED: {e}");
            ExitCode::FAILURE
        }
    }
}

fn keygen(key_file: &Path) -> CheckResult {
    let private = tunnel::generate_private_key().map_err(|e| e.to_string())?;
    let public = tunnel::public_key(&private).map_err(|e| e.to_string())?;
    write_private(key_file, &private)?;
    println!("{public}");
    Ok(())
}

fn write_private(path: &Path, contents: &str) -> CheckResult {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(contents.as_bytes()))
        .map_err(|e| format!("can't write {}: {e}", path.display()))
}

fn read(path: &Path) -> CheckResult<String> {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("can't read {}: {e}", path.display()))
}

async fn check(secrets: &Path, key_file: &Path, endpoint: Option<&str>) -> CheckResult {
    let connection = read(&secrets.join("connection.json"))?;
    let details = parse_connection(connection.as_bytes()).map_err(|e| e.to_string())?;
    let server_key = details
        .wireguard_server_public_key
        .ok_or("connection.json has no WireGuard server key")?;
    let cert = details
        .sunshine_cert_sha256
        .ok_or("connection.json has no Sunshine certificate fingerprint")?;
    let password = read(&secrets.join("sunshine-password"))?;
    let endpoint: SocketAddr = endpoint
        .unwrap_or("127.0.0.1:51820")
        .parse()
        .map_err(|e| format!("odd endpoint: {e}"))?;
    let peer = WireGuardPeer {
        endpoint,
        server_public_key: server_key,
        client_private_key: read(key_file)?,
        client_address: CLIENT_ADDRESS,
        server_address: SERVER_ADDRESS,
        mtu: TUNNEL_MTU,
    };

    let any_port = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let tunnel = Tunnel::open_with(
        &peer,
        &[
            Forward::tcp(any_port, SUNSHINE_API_PORT),
            Forward::tcp(any_port, NVHTTP_PORT),
        ],
    )
    .map_err(|e| e.to_string())?;
    tunnel
        .wait_until_up(HANDSHAKE_TIMEOUT)
        .await
        .map_err(|e| e.to_string())?;
    println!("ok: WireGuard handshake with {endpoint}");
    let result = check_through(&tunnel, &password, &cert).await;
    tunnel.close().await;
    result
}

async fn check_through(tunnel: &Tunnel, password: &str, cert: &str) -> CheckResult {
    let api = tunnel
        .local_addr(Protocol::Tcp, SUNSHINE_API_PORT)
        .ok_or("the tunnel has no API port")?;
    let sunshine = |password: &str, cert: &str| {
        SunshineApi::new(api, SUNSHINE_USER, password, Some(cert)).map_err(|e| e.to_string())
    };

    let pending = sunshine(password, cert)?
        .pending()
        .await
        .map_err(|e| format!("GET /api/pin: {e}"))?;
    if !pending.is_empty() {
        return Err(format!("expected no pairing requests, got {pending:?}"));
    }
    println!("ok: GET /api/pin through the tunnel, with the recorded certificate: no requests");
    let clients = sunshine(password, cert)?
        .clients()
        .await
        .map_err(|e| format!("GET /api/clients/list: {e}"))?;
    println!("ok: GET /api/clients/list: {} paired", clients.len());

    let wrong_cert = "00".repeat(32);
    match sunshine(password, &wrong_cert)?.pending().await {
        Err(SunshineError::WrongCertificate) => {
            println!("ok: another certificate fingerprint is refused before the login is sent")
        }
        other => {
            return Err(format!(
                "a wrong fingerprint should be refused, got {other:?}"
            ))
        }
    }
    match sunshine("not-the-password", cert)?.pending().await {
        Err(SunshineError::Unauthorized) => println!("ok: a wrong password is refused"),
        other => return Err(format!("a wrong password should be refused, got {other:?}")),
    }

    // What Moonlight asks first, on its own port.
    let nvhttp = tunnel
        .local_addr(Protocol::Tcp, NVHTTP_PORT)
        .ok_or("the tunnel has no HTTP port")?;
    let reply = server_info(nvhttp)
        .await
        .map_err(|e| format!("GET /serverinfo on {NVHTTP_PORT}: {e}"))?;
    let status = reply.lines().next().unwrap_or_default();
    if !status.contains(" 200 ") || !reply.contains("<root") {
        return Err(format!(
            "GET /serverinfo on {NVHTTP_PORT}: unexpected reply: {status}"
        ));
    }
    println!("ok: GET /serverinfo on Moonlight's port {NVHTTP_PORT}: {status}");
    Ok(())
}

async fn server_info(addr: SocketAddr) -> std::io::Result<String> {
    let exchange = async {
        let mut stream = TcpStream::connect(addr).await?;
        stream
            .write_all(
                format!(
                    "GET /serverinfo HTTP/1.1\r\nHost: {SERVER_ADDRESS}:{NVHTTP_PORT}\r\n\
                     Connection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await?;
        // Read to the end of the XML, in case the server keeps the
        // connection open anyway.
        let mut reply = Vec::new();
        let mut chunk = [0u8; 4096];
        while !String::from_utf8_lossy(&reply).contains("</root>") {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            reply.extend_from_slice(&chunk[..n]);
        }
        Ok(String::from_utf8_lossy(&reply).into_owned())
    };
    tokio::time::timeout(Duration::from_secs(20), exchange)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "no answer in 20 s"))?
}
