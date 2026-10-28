//! HTTPS to a paired phone, trusting only the certificate fingerprint it gave at pairing (pinned_https.py).
//! A tiny HTTP/1.1 client: the phone only ever answers with Content-Length or a stream that runs until close.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_REPLY_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Tls(rustls::Error),
    /// Something answered, but not with the certificate the phone paired with.
    PinMismatch,
    Http(u16),
    Bad(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Tls(e) => write!(f, "TLS: {e}"),
            Error::PinMismatch => write!(f, "the phone's certificate doesn't match the one it paired with"),
            Error::Http(code) => write!(f, "HTTP {code}"),
            Error::Bad(s) => write!(f, "{s}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        // rustls reports a failed verify as an io error wrapping its own
        if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
            if matches!(inner, rustls::Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure)) {
                return Error::PinMismatch;
            }
        }
        Error::Io(e)
    }
}

#[derive(Debug)]
struct Pin {
    sha256: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pin {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _intermediates: &[CertificateDer<'_>],
                          _server_name: &ServerName<'_>, _ocsp: &[u8], _now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        // No CA or hostname checks: the phone's certificate is self-signed and the pin replaces both
        if crate::util::same(&crate::util::sha256_hex(end_entity.as_ref()), &self.sha256) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct)
                              -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct)
                              -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// A paired phone at one address: its token and certificate pin.
#[derive(Clone, Debug)]
pub struct PhoneAuth {
    pub host: String,
    pub token: String,
    pub pin: String,
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: BufReader<StreamOwned<ClientConnection, TcpStream>>,
    socket: TcpStream,
    length: Option<usize>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// A handle that ends a read blocked on another thread.
    pub fn aborter(&self) -> std::io::Result<TcpStream> {
        self.socket.try_clone()
    }

    pub fn json(mut self) -> Result<Value, Error> {
        let mut raw = Vec::new();
        let limit = self.length.unwrap_or(MAX_REPLY_BYTES).min(MAX_REPLY_BYTES + 1);
        (&mut self.body).take(limit as u64).read_to_end(&mut raw)?;
        if raw.len() > MAX_REPLY_BYTES {
            return Err(Error::Bad("reply too big".into()));
        }
        serde_json::from_slice(&raw).map_err(|e| Error::Bad(e.to_string()))
    }
}

pub fn abort(socket: &TcpStream) {
    let _ = socket.shutdown(Shutdown::Both);
}

fn tls_config(pin: &str) -> Result<Arc<rustls::ClientConfig>, Error> {
    if !crate::util::is_fingerprint(pin) {
        return Err(Error::Bad("no certificate pin for this phone".into()));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(Pin { sha256: pin.to_string(), provider: provider.clone() });
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(Error::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

impl PhoneAuth {
    /// One request on a fresh connection; the body is left unread for the caller.
    pub fn request(&self, port: u16, method: &str, path: &str, body: Option<&Value>, timeout: Duration) -> Result<Response, Error> {
        let addr = (self.host.as_str(), port).to_socket_addrs()?.next().ok_or_else(|| Error::Bad("no address".into()))?;
        let socket = TcpStream::connect_timeout(&addr, timeout)?;
        socket.set_read_timeout(Some(timeout))?;
        socket.set_write_timeout(Some(timeout))?;
        socket.set_nodelay(true)?;
        let server_name = ServerName::try_from(self.host.clone()).map_err(|e| Error::Bad(e.to_string()))?;
        let conn = ClientConnection::new(tls_config(&self.pin)?, server_name).map_err(Error::Tls)?;
        let mut tls = StreamOwned::new(conn, socket.try_clone()?);
        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {}:{port}\r\nConnection: close\r\n", self.host);
        if path != "/v1/hello" {
            head += &format!("Authorization: Bearer {}\r\n", self.token);
        }
        if body.is_some() {
            head += &format!("Content-Type: application/json\r\nContent-Length: {}\r\n", payload.len());
        }
        head += "\r\n";
        tls.write_all(head.as_bytes())?;
        tls.write_all(payload.as_bytes())?;
        tls.flush()?;

        let mut reader = BufReader::with_capacity(64 * 1024, tls);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let status: u16 = line.split_whitespace().nth(1).and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::Bad(format!("not HTTP: {line:?}")))?;
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        let length = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok());
        Ok(Response { status, headers, body: reader, socket, length })
    }

    /// A JSON request whose answer must be 200.
    pub fn json(&self, port: u16, method: &str, path: &str, body: Option<&Value>, timeout: Duration) -> Result<Value, Error> {
        let r = self.request(port, method, path, body, timeout)?;
        if r.status != 200 {
            return Err(Error::Http(r.status));
        }
        r.json()
    }
}
