//! A client for Sunshine's pairing API, the REST API behind its web UI on
//! port 47990 (checked against `src/confighttp.cpp` at the Sunshine version
//! the playbook installs).
//!
//! - `GET /api/pin` lists the pairing requests waiting for a PIN.
//! - `POST /api/pin` gives one its PIN; the answer waits until Moonlight
//!   has finished the handshake, up to Sunshine's `ping_timeout`.
//! - `DELETE /api/pin` cancels one.
//! - `GET /api/clients/list` lists the paired clients.
//!
//! The login is Basic auth. Requests carry no `Origin` or `Referer`, which
//! is how Sunshine tells a program from a browser and lets it past its
//! CSRF check without a token.
//!
//! Sunshine's certificate is self-signed. Through the WireGuard tunnel the
//! tunnel has already proved it's the machine; without it the password
//! crosses the internet, so the certificate must match the SHA-256 the
//! setup recorded.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::Deserialize;
use serde_json::json;

/// What the verifier says when the certificate isn't the pinned one.
const WRONG_CERTIFICATE: &str = "Sunshine's certificate isn't the one recorded for this machine";
/// Most requests answer at once.
const TIMEOUT: Duration = Duration::from_secs(20);
/// Submitting a PIN waits for Moonlight's whole pairing handshake.
const PIN_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SunshineError {
    #[error("Sunshine didn't accept the app's login")]
    Unauthorized,
    #[error("Sunshine doesn't take API calls from this address")]
    Forbidden,
    #[error("{WRONG_CERTIFICATE}, so the app won't send it the password")]
    WrongCertificate,
    #[error("couldn't reach Sunshine: {0}")]
    Unreachable(String),
    #[error("Sunshine said: {0}")]
    Api(String),
    #[error("{0}")]
    Invalid(String),
}

pub type SunshineResult<T> = Result<T, SunshineError>;

/// A pairing request waiting for its PIN.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PendingPairing {
    /// 32 hex characters.
    pub id: String,
    /// The name Moonlight gave itself.
    pub name: String,
    /// Where the request came from: 10.213.37.2 through the tunnel.
    pub address: String,
}

/// A client Sunshine has paired.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PairedClient {
    pub name: String,
    pub uuid: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

pub struct SunshineApi {
    base: String,
    user: String,
    password: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for SunshineApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SunshineApi")
            .field("base", &self.base)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl SunshineApi {
    /// The API at `addr`. With `cert_sha256` (64 hex characters, colons
    /// allowed) the certificate must match it; without, any certificate
    /// will do, which is only safe through the tunnel.
    pub fn new(
        addr: SocketAddr,
        user: &str,
        password: &str,
        cert_sha256: Option<&str>,
    ) -> SunshineResult<Self> {
        let pin = cert_sha256.map(parse_sha256).transpose()?;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| SunshineError::Invalid(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedCertificate { pin, provider }))
            .with_no_client_auth();
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            // Sunshine redirects to its welcome page before it has a login;
            // nothing to follow there.
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| SunshineError::Invalid(describe(&e)))?;
        Ok(Self {
            base: format!("https://{addr}/api"),
            user: user.into(),
            password: password.into(),
            http,
        })
    }

    pub async fn pending(&self) -> SunshineResult<Vec<PendingPairing>> {
        #[derive(Deserialize)]
        struct Reply {
            #[serde(default)]
            pairings: Vec<PendingPairing>,
        }
        let reply: Reply = self
            .call(self.http.get(self.url("pin")).timeout(TIMEOUT))
            .await?;
        Ok(reply.pairings)
    }

    /// Gives a waiting request its PIN, under `name`. `false` when Sunshine
    /// turned it down: a wrong PIN, or Moonlight gave up meanwhile.
    pub async fn submit_pin(
        &self,
        pairing_id: &str,
        pin: &str,
        name: &str,
    ) -> SunshineResult<bool> {
        check_pairing_id(pairing_id)?;
        if pin.len() != 4 || !pin.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SunshineError::Invalid("the PIN is 4 digits".into()));
        }
        let body = json!({ "pairing_id": pairing_id, "pin": pin, "name": client_name(name) });
        let request = self.http.post(self.url("pin")).json(&body);
        Ok(self
            .call::<Status>(request.timeout(PIN_TIMEOUT))
            .await?
            .status)
    }

    /// Cancels a waiting request. `false` when it was no longer waiting.
    pub async fn cancel(&self, pairing_id: &str) -> SunshineResult<bool> {
        check_pairing_id(pairing_id)?;
        let body = json!({ "pairing_id": pairing_id });
        let request = self.http.delete(self.url("pin")).json(&body);
        Ok(self.call::<Status>(request.timeout(TIMEOUT)).await?.status)
    }

    pub async fn clients(&self) -> SunshineResult<Vec<PairedClient>> {
        #[derive(Deserialize)]
        struct Reply {
            #[serde(default)]
            named_certs: Vec<PairedClient>,
        }
        let request = self.http.get(self.url("clients/list")).timeout(TIMEOUT);
        Ok(self.call::<Reply>(request).await?.named_certs)
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base)
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> SunshineResult<T> {
        let response = request
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .map_err(|e| {
                let text = describe(&e);
                if text.contains(WRONG_CERTIFICATE) {
                    SunshineError::WrongCertificate
                } else {
                    SunshineError::Unreachable(text)
                }
            })?;
        let status = response.status();
        match status.as_u16() {
            401 => return Err(SunshineError::Unauthorized),
            403 => return Err(SunshineError::Forbidden),
            _ => {}
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| SunshineError::Unreachable(describe(&e)))?;
        if !status.is_success() {
            #[derive(Deserialize)]
            struct Failure {
                error: String,
            }
            let message = serde_json::from_slice::<Failure>(&body)
                .map(|f| f.error)
                .unwrap_or_else(|_| format!("HTTP {status}"));
            return Err(SunshineError::Api(message));
        }
        serde_json::from_slice(&body)
            .map_err(|e| SunshineError::Api(format!("an answer the app can't read: {e}")))
    }
}

#[derive(Deserialize)]
struct Status {
    status: bool,
}

fn check_pairing_id(id: &str) -> SunshineResult<()> {
    if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(SunshineError::Invalid(format!("{id:?} isn't a pairing id")))
    }
}

/// Sunshine takes 1 to 128 bytes.
fn client_name(name: &str) -> &str {
    let mut end = name.len().min(128);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

fn parse_sha256(text: &str) -> SunshineResult<[u8; 32]> {
    let hex: String = text.chars().filter(|c| *c != ':').collect();
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| {
            hex.get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
        })
        .collect::<Option<Vec<u8>>>();
    bytes
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or_else(|| SunshineError::Invalid("the certificate fingerprint isn't a SHA-256".into()))
}

/// The error and everything that caused it, on one line.
fn describe(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        // An io::Error shows its own cause in its message but doesn't hand
        // it out as the source.
        if let Some(inner) = cause
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.get_ref())
        {
            text = format!("{text}: {inner}");
        } else {
            text = format!("{text}: {cause}");
        }
        source = cause.source();
    }
    text
}

/// A 4-digit PIN for pairing.
pub fn new_pin() -> SunshineResult<String> {
    loop {
        let mut bytes = [0u8; 2];
        aws_lc_rs::rand::fill(&mut bytes)
            .map_err(|_| SunshineError::Invalid("no randomness for a PIN".into()))?;
        let n = u16::from_le_bytes(bytes);
        // 60000 is the largest multiple of 10000 under 65536, so every PIN
        // is equally likely.
        if n < 60000 {
            return Ok(format!("{:04}", n % 10000));
        }
    }
}

/// Accepts Sunshine's self-signed certificate when it is the pinned one
/// (or any, when nothing is pinned), and still checks the handshake was
/// signed with its key.
#[derive(Debug)]
struct PinnedCertificate {
    pin: Option<[u8; 32]>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedCertificate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Some(pin) = self.pin {
            let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, end_entity);
            if digest.as_ref() != pin {
                return Err(rustls::Error::General(WRONG_CERTIFICATE.into()));
            }
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::fake::FakeSunshine;
    use serde_json::Value;

    const ID: &str = "0123456789abcdef0123456789ABCDEF";

    #[tokio::test]
    async fn pending_requests_with_basic_auth_and_no_browser_headers() {
        let sunshine = FakeSunshine::start(|req| match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/api/pin") => (
                200,
                json!({"pairings": [{"id": ID, "name": "roth", "address": "10.213.37.2"}]}),
            ),
            _ => (404, json!({})),
        })
        .await;
        let api = sunshine.api("rega", "s3cret", true);
        let pending = api.pending().await.unwrap();
        assert_eq!(
            pending,
            vec![PendingPairing {
                id: ID.into(),
                name: "roth".into(),
                address: "10.213.37.2".into()
            }]
        );
        let seen = sunshine.requests();
        let req = &seen[0];
        // "rega:s3cret"
        assert_eq!(
            req.header("authorization").as_deref(),
            Some("Basic cmVnYTpzM2NyZXQ=")
        );
        assert_eq!(req.header("origin"), None);
        assert_eq!(req.header("referer"), None);
    }

    #[tokio::test]
    async fn submit_and_cancel_send_json_and_read_the_status() {
        let sunshine = FakeSunshine::start(|req| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
            match (req.method.as_str(), req.path.as_str()) {
                ("POST", "/api/pin") => (200, json!({"status": body["pin"] == "1234"})),
                ("DELETE", "/api/pin") => (200, json!({"status": true})),
                _ => (404, json!({})),
            }
        })
        .await;
        let api = sunshine.api("rega", "pw", true);
        assert!(api.submit_pin(ID, "1234", "ReGa on desk").await.unwrap());
        assert!(!api.submit_pin(ID, "9999", "ReGa on desk").await.unwrap());
        assert!(api.cancel(ID).await.unwrap());

        let seen = sunshine.requests();
        assert_eq!(seen.len(), 3);
        for req in &seen {
            assert_eq!(
                req.header("content-type").as_deref(),
                Some("application/json")
            );
            assert_eq!(req.header("origin"), None);
            assert_eq!(req.header("referer"), None);
        }
        let body: Value = serde_json::from_slice(&seen[0].body).unwrap();
        assert_eq!(
            body,
            json!({"pairing_id": ID, "pin": "1234", "name": "ReGa on desk"})
        );
        let body: Value = serde_json::from_slice(&seen[2].body).unwrap();
        assert_eq!(body, json!({"pairing_id": ID}));
    }

    #[tokio::test]
    async fn bad_input_never_leaves_the_app() {
        let sunshine = FakeSunshine::start(|_| (200, json!({"status": true}))).await;
        let api = sunshine.api("rega", "pw", true);
        for id in [
            "",
            "0123",
            "0123456789abcdef0123456789abcdeg",
            &format!("{ID}0"),
        ] {
            assert!(matches!(
                api.submit_pin(id, "1234", "x").await,
                Err(SunshineError::Invalid(_))
            ));
            assert!(matches!(
                api.cancel(id).await,
                Err(SunshineError::Invalid(_))
            ));
        }
        for pin in ["", "123", "12345", "12a4"] {
            assert!(matches!(
                api.submit_pin(ID, pin, "x").await,
                Err(SunshineError::Invalid(_))
            ));
        }
        assert!(sunshine.requests().is_empty());
        // Long names are cut to what Sunshine takes.
        let long = "é".repeat(100);
        assert!(api.submit_pin(ID, "1234", &long).await.unwrap());
        let body: Value = serde_json::from_slice(&sunshine.requests()[0].body).unwrap();
        assert_eq!(body["name"].as_str().unwrap().len(), 128);
    }

    #[tokio::test]
    async fn refusals_and_errors() {
        let sunshine = FakeSunshine::start(|req| match req.path.as_str() {
            "/api/clients/list" => (401, json!({"status_code": 401, "error": "Unauthorized"})),
            "/api/pin" if req.method == "GET" => (403, json!({})),
            _ => (
                400,
                json!({"status_code": 400, "status": false, "error": "Content type mismatch"}),
            ),
        })
        .await;
        let api = sunshine.api("rega", "wrong", true);
        assert_eq!(api.clients().await, Err(SunshineError::Unauthorized));
        assert_eq!(api.pending().await, Err(SunshineError::Forbidden));
        assert_eq!(
            api.cancel(ID).await,
            Err(SunshineError::Api("Content type mismatch".into()))
        );
    }

    #[tokio::test]
    async fn clients_list() {
        let sunshine = FakeSunshine::start(|_| {
            (
                200,
                json!({"status": true, "named_certs": [
                    {"name": "ReGa on desk", "uuid": "u1", "enabled": true},
                    {"name": "Phone", "uuid": "u2"},
                ]}),
            )
        })
        .await;
        let clients = sunshine.api("rega", "pw", true).clients().await.unwrap();
        assert_eq!(clients.len(), 2);
        assert_eq!(clients[0].name, "ReGa on desk");
        assert!(clients[1].enabled);
    }

    #[tokio::test]
    async fn the_pinned_certificate_is_required_when_given() {
        let sunshine = FakeSunshine::start(|_| (200, json!({"pairings": []}))).await;
        // The right fingerprint, with colons and in capitals as some tools
        // print it.
        let colons = sunshine
            .cert_sha256()
            .as_bytes()
            .chunks(2)
            .map(|c| std::str::from_utf8(c).unwrap().to_uppercase())
            .collect::<Vec<_>>()
            .join(":");
        for pin in [sunshine.cert_sha256(), colons] {
            let api = SunshineApi::new(sunshine.addr(), "rega", "pw", Some(&pin)).unwrap();
            assert_eq!(api.pending().await.unwrap(), vec![]);
        }
        let wrong = "ab".repeat(32);
        let api = SunshineApi::new(sunshine.addr(), "rega", "pw", Some(&wrong)).unwrap();
        assert_eq!(api.pending().await, Err(SunshineError::WrongCertificate));
        // Only the requests with the right certificate got through, so the
        // password never went to the wrong one.
        assert_eq!(sunshine.requests().len(), 2);
        assert!(SunshineApi::new(sunshine.addr(), "rega", "pw", Some("abc")).is_err());
    }

    #[tokio::test]
    async fn nobody_listening() {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = free.local_addr().unwrap();
        drop(free);
        let api = SunshineApi::new(addr, "rega", "pw", None).unwrap();
        assert!(matches!(
            api.pending().await,
            Err(SunshineError::Unreachable(_))
        ));
        assert!(!format!("{api:?}").contains("pw"));
    }

    #[test]
    fn pins_are_four_digits() {
        let pins: Vec<String> = (0..200).map(|_| new_pin().unwrap()).collect();
        assert!(pins
            .iter()
            .all(|p| p.len() == 4 && p.bytes().all(|b| b.is_ascii_digit())));
        let distinct: std::collections::HashSet<_> = pins.iter().collect();
        assert!(distinct.len() > 100);
    }
}
