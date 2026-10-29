//! Pages for other devices on the network, over HTTPS (telescope.browser_server's TlsServer and PageHandler): the certificate every
//! such server shares, the server with its connection caps and the headers every page gets. The On Air sign is the first
//! thing served and the Browser camera's page is the next. The link crate implements it.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One request, as far as a page cares: what was asked for and the headers it came with.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// "GET" or "HEAD", the only ones that get this far.
    pub method: String,
    /// The path and query as sent ("/state?after=3").
    pub target: String,
    /// Names as sent; look them up with header().
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn path(&self) -> &str {
        self.target.split_once('?').map_or(self.target.as_str(), |(path, _)| path)
    }

    pub fn query(&self) -> &str {
        self.target.split_once('?').map_or("", |(_, query)| query)
    }

    /// The value of the first query parameter with this name, still percent-encoded.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query().split('&').filter_map(|pair| pair.split_once('=').or(Some((pair, "")))).find(|(k, _)| *k == name).map(|(_, v)| v)
    }

    /// A header's value by name, whatever the case it was sent in.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// What to answer. The server adds the headers every page gets (Cache-Control, the content security policy...) and leaves
/// the body off an answer to HEAD.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn new(status: u16, body: impl Into<Vec<u8>>, content_type: &str) -> Reply {
        Reply { status, content_type: content_type.into(), body: body.into() }
    }

    /// A short plain-text answer (_reply's default content type).
    pub fn text(status: u16, body: &str) -> Reply {
        Reply::new(status, body, "text/plain; charset=utf-8")
    }
}

/// What a page does with each request. Called on the connection's own thread, so it may wait (a page held open until
/// something changes) without holding up anything else.
pub trait PageHandler: Send + Sync {
    fn handle(&self, request: &Request) -> Reply;
}

/// How far devices got, for telling a firewall from a certificate warning from an old link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    /// A device reached this computer.
    Connections,
    /// A browser hung up during the TLS handshake (it hasn't accepted the certificate yet).
    TlsFailed,
    /// A browser loaded the page.
    Pages,
    /// A browser was refused with an old or wrong link.
    Refused,
    /// A browser connected.
    Browsers,
}

impl Step {
    pub const ALL: [Step; 5] = [Step::Connections, Step::TlsFailed, Step::Pages, Step::Refused, Step::Browsers];

    /// What the log says the first time it happens.
    pub fn text(self) -> &'static str {
        match self {
            Step::Connections => "a device reached this computer",
            Step::TlsFailed => "a browser hung up during the TLS handshake (it hasn't accepted the certificate yet)",
            Step::Pages => "a browser loaded the page",
            Step::Refused => "a browser was refused with an old or wrong link",
            Step::Browsers => "a browser connected",
        }
    }
}

/// Counts only: no addresses or tokens. Each step is logged the first time it happens.
#[derive(Debug)]
pub struct ServerStats {
    pub port: u16,
    pub wanted_port: u16,
    /// wanted_port was taken, so port is whatever was free.
    pub fell_back: bool,
    /// What its log lines start with.
    pub label: String,
    pub started: Instant,
    counts: Mutex<HashMap<Step, u64>>,
}

impl ServerStats {
    pub fn new(port: u16, wanted_port: u16, fell_back: bool, label: &str) -> ServerStats {
        ServerStats { port, wanted_port, fell_back, label: label.into(), started: Instant::now(), counts: Mutex::default() }
    }

    pub fn note(&self, step: Step) {
        let first = {
            let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
            let n = counts.entry(step).or_insert(0);
            *n += 1;
            *n == 1
        };
        if first {
            log::info!("{}: {}", self.label, step.text());
        }
    }

    pub fn count(&self, step: Step) -> u64 {
        self.counts.lock().unwrap_or_else(|e| e.into_inner()).get(&step).copied().unwrap_or(0)
    }

    pub fn nothing_arrived(&self) -> bool {
        self.count(Step::Connections) == 0
    }
}

/// The certificate and key (PEM files) a server presents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Where and as what a server listens.
#[derive(Clone, Debug)]
pub struct Serve {
    /// 0 for any free one. A port that's taken falls back to any free one, which stats().fell_back says.
    pub port: u16,
    /// The address to listen on; "" for all of them.
    pub host: String,
    /// What its log lines start with.
    pub label: String,
}

/// A running server. It stops when stop() is called or it's dropped.
pub trait WebServer: Send + Sync {
    /// The port it listens on.
    fn port(&self) -> u16;
    fn stats(&self) -> Arc<ServerStats>;
    fn stop(&self);
}

pub trait Web: Send + Sync {
    /// The addresses another device could reach this computer on, best first (ip_utils.get_pairing_addresses).
    fn addresses(&self) -> Vec<String>;
    /// The certificate in folder, made again when it's missing, unreadable or about to run out; the same one for as long
    /// as it's valid, so a browser that was told to trust it once keeps doing so. It names `hosts` (IP addresses) and
    /// localhost. Err says why it couldn't be made.
    fn certificate(&self, folder: &Path, hosts: &[String]) -> Result<Certificate, String>;
    /// Starts serving `handler` over TLS 1.2 or newer. Err says why it couldn't start.
    fn serve(&self, certificate: &Certificate, serve: &Serve, handler: Arc<dyn PageHandler>) -> Result<Box<dyn WebServer>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(target: &str) -> Request {
        Request { method: "GET".into(), target: target.into(), headers: vec![("Authorization".into(), "Bearer abc".into())] }
    }

    #[test]
    fn a_request_is_taken_apart() {
        let r = request("/state?after=3&x");
        assert_eq!((r.path(), r.query()), ("/state", "after=3&x"));
        assert_eq!((r.param("after"), r.param("x"), r.param("y")), (Some("3"), Some(""), None));
        assert_eq!(request("/").query(), "");
        assert_eq!(r.header("authorization"), Some("Bearer abc"));
        assert_eq!(r.header("Cookie"), None);
    }

    #[test]
    fn the_first_of_each_step_is_logged_and_all_are_counted() {
        let stats = ServerStats::new(8768, 8768, false, "On Air");
        assert!(stats.nothing_arrived());
        stats.note(Step::Connections);
        stats.note(Step::Connections);
        stats.note(Step::Refused);
        assert_eq!((stats.count(Step::Connections), stats.count(Step::Refused), stats.count(Step::Pages)), (2, 1, 0));
        assert!(!stats.nothing_arrived());
        assert!(Step::ALL.iter().all(|s| !s.text().is_empty()));
    }
}
