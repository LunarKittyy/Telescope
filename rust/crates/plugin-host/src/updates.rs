//! Whether added plugins have a newer version out, for those whose plugin.json "repo" is on GitHub and that publish their builds as
//! GitHub Releases (plugin_updates.py). The network is a Fetch, so tests stand in for GitHub.
//!
//! It only asks GitHub's API for each repo's latest release, at most once a day per repo, and keeps the answers (with their ETag, so asking
//! again costs nothing against the rate limit when nothing changed) in plugin-updates.json next to the settings. Being offline or rate
//! limited is normal: it waits (an hour, doubling up to a day, or until GitHub says the limit resets) and never fails loudly.

use serde_json::{json, Map, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CHECK_INTERVAL_S: f64 = 24.0 * 60.0 * 60.0;
/// After a failure; doubles with each one in a row.
pub const WAIT_FIRST_S: f64 = 60.0 * 60.0;
pub const WAIT_MAX_S: f64 = 24.0 * 60.0 * 60.0;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
pub const MAX_REPLY_BYTES: usize = 512 * 1024;
/// What the plugins folder takes unpacked, so a .zip is never bigger.
pub const MAX_ZIP_BYTES: u64 = crate::package::MAX_BYTES;
const MAX_ASSETS: usize = 30;

/// One HTTP answer. Any status comes back as a Reply; an Err is only for not getting one (offline, a timeout, TLS).
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Box<dyn Read + Send>,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// The network, for GitHub's API and release downloads.
pub trait Fetch: Send + Sync {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Reply, String>;
}

/// The real network over HTTPS, following redirects (release downloads go through one).
pub struct Https;

impl Fetch for Https {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Reply, String> {
        // The system's certificate store, as every download of Telescope's own (telescope.web.urlopen)
        let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
        let config = ureq::Agent::config_builder().tls_config(tls).timeout_resolve(Some(REQUEST_TIMEOUT)).timeout_connect(Some(REQUEST_TIMEOUT)).timeout_send_request(Some(REQUEST_TIMEOUT)).timeout_recv_response(Some(REQUEST_TIMEOUT)).timeout_per_read(Some(REQUEST_TIMEOUT)).http_status_as_error(false)
            .user_agent(format!("Telescope/{}", env!("CARGO_PKG_VERSION"))).build();
        let agent = ureq::Agent::new_with_config(config);
        let mut request = agent.get(url);
        for (k, v) in headers {
            request = request.header(*k, *v);
        }
        let response = request.call().map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let headers = response.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect();
        Ok(Reply { status, headers, body: Box::new(response.into_body().into_reader()) })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// GitHub's digest of it, "" for releases from before it gave one.
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    /// Its page on GitHub.
    pub page: String,
    /// The .zip files it has.
    pub assets: Vec<Asset>,
}

impl Release {
    /// The .zip of plugin name: <name>.zip, else one starting with its name, else the only one.
    pub fn zip_for(&self, name: &str) -> Option<&Asset> {
        let exact: Vec<&Asset> = self.assets.iter().filter(|a| a.name.to_lowercase() == format!("{name}.zip")).collect();
        let starting: Vec<&Asset> = self.assets.iter().filter(|a| a.name.to_lowercase().starts_with(name)).collect();
        let all: Vec<&Asset> = self.assets.iter().collect();
        [exact, starting, all].into_iter().find(|found| found.len() == 1).map(|found| found[0])
    }

    fn to_json(&self) -> Value {
        json!({"tag": self.tag, "page": self.page,
               "assets": self.assets.iter().map(|a| json!({"name": a.name, "url": a.url, "size": a.size, "sha256": a.sha256})).collect::<Vec<_>>()})
    }

    fn from_json(data: &Value) -> Option<Release> {
        let assets = data.get("assets").and_then(Value::as_array).map(|list| list.iter().map(|a| Some(Asset {
            name: a.get("name")?.as_str()?.to_string(),
            url: a.get("url")?.as_str()?.to_string(),
            size: a.get("size")?.as_u64()?,
            sha256: a.get("sha256").and_then(Value::as_str).unwrap_or("").to_string(),
        })).collect::<Option<Vec<_>>>()).unwrap_or(Some(Vec::new()))?;
        Some(Release { tag: data.get("tag")?.as_str()?.to_string(), page: data.get("page")?.as_str()?.to_string(), assets })
    }
}

/// plugin-updates.json next to the settings.
pub fn cache_path() -> PathBuf {
    telescope_core::config::config_path().parent().map(|p| p.join("plugin-updates.json")).unwrap_or_else(|| PathBuf::from("plugin-updates.json"))
}

/// "owner/repo" for a https://github.com/owner/repo address, else None.
pub fn github_repo(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let (owner, repo) = rest.split_once('/')?;
    let owner_ok = (1..=39).contains(&owner.len()) && owner.as_bytes()[0].is_ascii_alphanumeric()
        && owner.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-');
    let repo = repo.strip_suffix('/').unwrap_or(repo);
    let repo = match repo.strip_suffix(".git") {
        Some(r) if !r.is_empty() => r,
        _ => repo,
    };
    let repo_ok = (1..=100).contains(&repo.len()) && repo.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c));
    (owner_ok && repo_ok).then(|| format!("{owner}/{repo}"))
}

/// (the numbers, whether more follows them like -beta) for "1.2", "v1.2.0-beta" and the like, else None.
pub fn parse_version(text: &str) -> Option<(Vec<u64>, bool)> {
    let t = text.trim();
    let t = t.strip_prefix(['v', 'V']).unwrap_or(t);
    let mut numbers = Vec::new();
    let mut rest = t;
    loop {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            if numbers.is_empty() {
                return None;
            }
            break;
        }
        numbers.push(rest[..digits].parse().ok()?);
        rest = &rest[digits..];
        match rest.strip_prefix('.') {
            Some(after) if after.starts_with(|c: char| c.is_ascii_digit()) => rest = after,
            _ => break,
        }
    }
    while numbers.len() > 1 && numbers.last() == Some(&0) {
        numbers.pop();
    }
    Some((numbers, !rest.trim_matches([' ', '.', '-', '+']).is_empty()))
}

/// Whether a release tagged tag is newer than the installed version; false when either can't be read.
pub fn is_newer(tag: &str, installed: &str) -> bool {
    let (Some(new), Some(old)) = (parse_version(tag), parse_version(installed)) else { return false };
    if new.0 != old.0 {
        return new.0 > old.0;
    }
    old.1 && !new.1
}

/// The release from GitHub's API reply, None when it isn't one this can use.
pub fn parse_release(raw: &[u8]) -> Option<Release> {
    let data: Value = serde_json::from_slice(raw).ok()?;
    let tag = data.get("tag_name")?.as_str()?;
    let page = data.get("html_url")?.as_str()?;
    if tag.is_empty() || tag.chars().count() > 100 || !page.starts_with("https://github.com/") {
        return None;
    }
    let mut assets = Vec::new();
    for a in data.get("assets").and_then(Value::as_array).into_iter().flatten().take(MAX_ASSETS) {
        let (Some(name), Some(url), Some(size)) = (a.get("name").and_then(Value::as_str), a.get("browser_download_url").and_then(Value::as_str),
                                                   a.get("size").and_then(Value::as_u64)) else { continue };
        if !name.to_lowercase().ends_with(".zip") || !url.starts_with("https://github.com/") || size == 0 || size > MAX_ZIP_BYTES {
            continue;
        }
        let digest = a.get("digest").and_then(Value::as_str).unwrap_or("");
        let sha = match digest.strip_prefix("sha256:") {
            Some(hex) if digest.len() == 71 => hex.to_lowercase(),
            _ => String::new(),
        };
        assets.push(Asset { name: name.into(), url: url.into(), size, sha256: sha });
    }
    Some(Release { tag: tag.into(), page: page.into(), assets })
}

fn number(v: Option<&Value>) -> f64 {
    v.and_then(Value::as_f64).filter(|_| !v.is_some_and(Value::is_boolean)).unwrap_or(0.0)
}

/// What the last checks found, by repo, and how long to leave GitHub alone. A file that's missing or broken is the same as no checks yet.
#[derive(Clone, Debug)]
pub struct Cache {
    pub path: PathBuf,
    /// "owner/repo": {"checked": time, "etag": str, "release": object or null}
    pub repos: Map<String, Value>,
    pub wait_until: f64,
    pub failures: u32,
    /// Plugin name: the tag auto-update installed last, never installed twice.
    pub auto_installed: Map<String, Value>,
}

impl Cache {
    pub fn open(path: impl Into<PathBuf>) -> Cache {
        let path = path.into();
        let mut cache = Cache { path, repos: Map::new(), wait_until: 0.0, failures: 0, auto_installed: Map::new() };
        let Some(data) = std::fs::read(&cache.path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return cache };
        let Value::Object(data) = data else { return cache };
        if let Some(Value::Object(repos)) = data.get("repos") {
            cache.repos = repos.iter().filter(|(_, v)| v.is_object()).map(|(k, v)| (k.clone(), v.clone())).collect();
        }
        cache.wait_until = number(data.get("wait_until"));
        cache.failures = number(data.get("failures")) as u32;
        if let Some(Value::Object(auto)) = data.get("auto_installed") {
            cache.auto_installed = auto.iter().filter(|(_, v)| v.is_string()).map(|(k, v)| (k.clone(), v.clone())).collect();
        }
        cache
    }

    pub fn release(&self, repo: &str) -> Option<Release> {
        self.repos.get(repo).and_then(|e| e.get("release")).filter(|r| !r.is_null()).and_then(Release::from_json)
    }

    pub fn due(&self, repo: &str, now: f64) -> bool {
        let checked = number(self.repos.get(repo).and_then(|e| e.get("checked")));
        // A clock set back counts as due too
        !(0.0..CHECK_INTERVAL_S).contains(&(now - checked))
    }

    pub fn auto_installed(&self, name: &str) -> Option<&str> {
        self.auto_installed.get(name).and_then(Value::as_str)
    }

    /// Writes it; a failure is logged and the next check just asks again.
    pub fn save(&self) {
        let data = json!({"repos": self.repos, "wait_until": self.wait_until, "failures": self.failures, "auto_installed": self.auto_installed});
        let tmp = self.path.with_extension("json.tmp");
        let result = self.path.parent().map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&tmp, data.to_string())).and_then(|_| std::fs::rename(&tmp, &self.path));
        if let Err(e) = result {
            log::warn!("Couldn't save the plugin update checks: {e}");
        }
    }
}

/// How long GitHub wants left alone after a 403 or 429: its Retry-After or rate limit reset, at least an hour, at most a day.
fn wait_after_limit(reply: &Reply, now: f64) -> f64 {
    let mut wait = WAIT_FIRST_S;
    if let Some(after) = reply.header("Retry-After").and_then(|v| v.trim().parse::<f64>().ok()) {
        wait = wait.max(after);
    }
    if let Some(reset) = reply.header("X-RateLimit-Reset").and_then(|v| v.trim().parse::<f64>().ok()) {
        wait = wait.max(reset - now);
    }
    wait.min(WAIT_MAX_S)
}

/// A 429, or a 403 that comes with GitHub's rate limit headers (a 403 alone is about the one repo).
fn rate_limited(reply: &Reply) -> bool {
    reply.status == 429 || (reply.status == 403 && (reply.header("X-RateLimit-Remaining") == Some("0") || reply.header("Retry-After").is_some_and(|v| !v.is_empty())))
}

fn failed(cache: &mut Cache, now: f64, why: &str) {
    cache.failures += 1;
    let wait = (WAIT_FIRST_S * 2f64.powi(cache.failures.saturating_sub(1).min(10) as i32)).min(WAIT_MAX_S);
    cache.wait_until = now + wait;
    log::info!("Couldn't check for plugin updates ({why}); trying again in {} min", (wait / 60.0) as u64);
}

pub fn api_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

/// Asks GitHub about each repo that's due, unless it's being left alone after a failure. Whatever goes wrong only makes it wait longer.
/// Saves the cache when it asked.
pub fn check(repos: &[String], cache: &mut Cache, now: f64, fetch: &dyn Fetch) {
    if now < cache.wait_until && cache.wait_until - now <= WAIT_MAX_S {
        return;
    }
    let mut sorted = repos.to_vec();
    sorted.sort();
    sorted.dedup();
    let mut asked = false;
    let mut stopped = false;
    for repo in sorted {
        if !cache.due(&repo, now) {
            continue;
        }
        asked = true;
        let entry = cache.repos.get(&repo).cloned().unwrap_or_else(|| json!({}));
        let etag = entry.get("etag").and_then(Value::as_str).unwrap_or("").to_string();
        let mut headers = vec![("Accept", "application/vnd.github+json"), ("X-GitHub-Api-Version", "2022-11-28")];
        if !etag.is_empty() {
            headers.push(("If-None-Match", etag.as_str()));
        }
        let mut reply = match fetch.get(&api_url(&repo), &headers) {
            Ok(r) => r,
            Err(e) => {
                failed(cache, now, &e);
                stopped = true;
                break;
            }
        };
        match reply.status {
            // Nothing new since the ETag, and it didn't count against the limit
            304 => {
                let mut e = entry.as_object().cloned().unwrap_or_default();
                e.insert("checked".into(), json!(now));
                cache.repos.insert(repo, Value::Object(e));
                continue;
            }
            _ if rate_limited(&reply) => {
                cache.wait_until = now + wait_after_limit(&reply, now);
                log::info!("GitHub is limiting plugin update checks; trying again in {} min", ((cache.wait_until - now) / 60.0) as u64);
                stopped = true;
                break;
            }
            // No release, or this repo is blocked or gone: ask again tomorrow, others now
            400..=499 => {
                if reply.status != 404 {
                    log::info!("GitHub said HTTP {} about {repo}", reply.status);
                }
                cache.repos.insert(repo, json!({"checked": now, "etag": "", "release": null}));
                continue;
            }
            200..=299 => {}
            other => {
                failed(cache, now, &format!("HTTP {other}"));
                stopped = true;
                break;
            }
        }
        let mut raw = Vec::new();
        if let Err(e) = (&mut reply.body).take(MAX_REPLY_BYTES as u64 + 1).read_to_end(&mut raw) {
            failed(cache, now, &e.to_string());
            stopped = true;
            break;
        }
        let release = if raw.len() <= MAX_REPLY_BYTES { parse_release(&raw) } else { None };
        if release.is_none() {
            log::info!("The latest release of {repo} couldn't be read");
        }
        let etag = if release.is_some() { reply.header("ETag").unwrap_or("").to_string() } else { String::new() };
        cache.repos.insert(repo, json!({"checked": now, "etag": etag, "release": release.map(|r| r.to_json())}));
    }
    if asked && !stopped {
        cache.wait_until = 0.0;
        cache.failures = 0;
    }
    if asked {
        cache.save();
    }
}

/// Why a release's .zip couldn't be downloaded, in English since it only goes to the log.
#[derive(Debug, PartialEq)]
pub struct DownloadError(pub String);

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Downloads a release's .zip into dest_dir as plugin.zip, checked against its size and (when GitHub gave one) its sha256.
pub fn download(asset: &Asset, dest_dir: &Path, fetch: &dyn Fetch) -> Result<PathBuf, DownloadError> {
    let name = &asset.name;
    if asset.size > MAX_ZIP_BYTES || !asset.url.starts_with("https://") {
        return Err(DownloadError(format!("{name} isn't a plugin .zip this downloads")));
    }
    let target = dest_dir.join("plugin.zip");
    let not_downloaded = |why: &str| DownloadError(format!("{name} couldn't be downloaded ({why})"));
    let mut reply = fetch.get(&asset.url, &[("Accept", "application/octet-stream")]).map_err(|e| not_downloaded(&e))?;
    if !(200..300).contains(&reply.status) {
        return Err(not_downloaded(&format!("HTTP {}", reply.status)));
    }
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 256 * 1024];
    loop {
        let n = reply.body.read(&mut chunk).map_err(|e| not_downloaded(&e.kind().to_string()))?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..n]);
        if data.len() as u64 > asset.size {
            return Err(DownloadError(format!("{name} is bigger than GitHub said")));
        }
    }
    if data.len() as u64 != asset.size {
        return Err(DownloadError(format!("{name} was cut short")));
    }
    if !asset.sha256.is_empty() && crate::package::sha256_hex(&data) != asset.sha256 {
        return Err(DownloadError(format!("{name} didn't match its checksum")));
    }
    std::fs::write(&target, data).map_err(|e| not_downloaded(&e.kind().to_string()))?;
    Ok(target)
}
