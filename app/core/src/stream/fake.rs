//! Test doubles: an HTTPS Sunshine API with a throwaway self-signed
//! certificate, which records every request and answers with a closure.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::sunshine::SunshineApi;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Names in lowercase.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }
}

type Handler = dyn Fn(&Request) -> (u16, Value) + Send + Sync;

pub struct FakeSunshine {
    addr: SocketAddr,
    cert_sha256: String,
    requests: Arc<Mutex<Vec<Request>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for FakeSunshine {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakeSunshine {
    pub async fn start(handler: impl Fn(&Request) -> (u16, Value) + Send + Sync + 'static) -> Self {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let der = cert.cert.der().clone();
        let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, &der);
        let cert_sha256 = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![der], key)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let server = {
            let requests = requests.clone();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let (acceptor, requests, handler) =
                        (acceptor.clone(), requests.clone(), handler.clone());
                    tokio::spawn(async move {
                        let Ok(mut tls) = acceptor.accept(stream).await else {
                            return;
                        };
                        let Some(request) = read_request(&mut tls).await else {
                            return;
                        };
                        requests.lock().unwrap().push(request.clone());
                        // The handler may block, like Sunshine holding a PIN
                        // request open.
                        let (status, body) = tokio::task::spawn_blocking(move || handler(&request))
                            .await
                            .unwrap();
                        let body = body.to_string();
                        let response = format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = tls.write_all(response.as_bytes()).await;
                        let _ = tls.shutdown().await;
                    });
                }
            })
        };
        Self {
            addr,
            cert_sha256,
            requests,
            server,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Lowercase hex, as the setup records it.
    pub fn cert_sha256(&self) -> String {
        self.cert_sha256.clone()
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// A client for it, pinned to its certificate or not.
    pub fn api(&self, user: &str, password: &str, pinned: bool) -> SunshineApi {
        let pin = pinned.then(|| self.cert_sha256());
        SunshineApi::new(self.addr, user, password, pin.as_deref()).unwrap()
    }
}

async fn read_request(
    stream: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
) -> Option<Request> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let path = first.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = data[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}
