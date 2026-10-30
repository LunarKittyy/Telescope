//! Update check, download and install (telescope/updates.py). The network is an Opener, so tests stand in for GitHub.
//!
//! Each release carries a manifest.json with the version, the build number and every file's SHA-256; builds compare by
//! build number, so switching channel never downgrades. Installing swaps the files through update_guard's journal.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use telescope_core::i18n::tr;

use crate::update_guard::{self, Mover, EXE_NAME, FAILED_EXE_NAME, OLD_EXE_NAME, PREVIOUS_DIR, STAGING_DIR};
use crate::version;

pub const CHANNELS: [&str; 2] = ["stable", "nightly"];
pub const WINDOWS_ASSET: &str = "Telescope-windows-portable.zip";
pub const WINDOWS_SETUP_ASSET: &str = "TelescopeSetup.exe";
pub const LINUX_ASSET: &str = "Telescope-linux.tar.gz";
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const CHUNK: usize = 256 * 1024;

pub fn manifest_url(channel: &str) -> String {
    if channel == "nightly" {
        format!("https://github.com/{}/releases/download/nightly/manifest.json", version::REPO)
    } else {
        format!("https://github.com/{}/releases/latest/download/manifest.json", version::REPO)
    }
}

/// Something the user should read: the message is the UI text.
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateError(pub String);

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

type Result<T> = std::result::Result<T, UpdateError>;

fn err<T>(text: &str) -> Result<T> {
    Err(UpdateError(tr(text)))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub version: String,
    pub build: i64,
    pub channel: String,
    pub version_name: String,
    pub notes: String,
    pub protocol: i64,
    pub assets: BTreeMap<String, Asset>,
}

impl Manifest {
    pub fn display_version(&self) -> String {
        if self.channel == "stable" { self.version.clone() } else { format!("{} {} {}", self.version, self.channel, self.build) }
    }
}

/// Nightly builds follow nightly; stable and source builds follow stable.
pub fn default_channel() -> &'static str {
    if version::channel() == "nightly" { "nightly" } else { "stable" }
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn parse_manifest(raw: &[u8]) -> Result<Manifest> {
    let parse = || -> Option<Manifest> {
        let data: Value = serde_json::from_slice(raw).ok()?;
        let mut assets = BTreeMap::new();
        for a in data.get("assets")?.as_array()? {
            let asset = Asset { name: text(&a["name"])?, url: text(&a["url"])?, sha256: text(&a["sha256"])?.to_lowercase(), size: u64::try_from(int(&a["size"])?).ok()? };
            if !asset.url.starts_with("https://") || asset.sha256.len() != 64 {
                return None;
            }
            assets.insert(asset.name.clone(), asset);
        }
        let version = text(data.get("version")?)?;
        Some(Manifest {
            version_name: data.get("versionName").and_then(text).unwrap_or_else(|| version.clone()),
            version,
            build: int(data.get("build")?)?,
            channel: text(data.get("channel")?)?,
            notes: data.get("notes").map_or(Some(String::new()), text)?,
            protocol: data.get("protocol").map_or(Some(0), int)?,
            assets,
        })
    };
    parse().map_or_else(|| err("The update information couldn't be read."), Ok)
}

/// One HTTP answer, any status.
pub struct Reply {
    pub status: u16,
    pub body: Box<dyn Read + Send>,
}

/// Not getting an answer at all.
#[derive(Debug)]
pub enum NetError {
    /// GitHub's certificate didn't check out (security software or a network that inspects HTTPS).
    Certificate,
    Other(String),
}

pub trait Opener: Send + Sync {
    fn open(&self, url: &str) -> std::result::Result<Reply, NetError>;
}

impl<F: Fn(&str) -> std::result::Result<Reply, NetError> + Send + Sync> Opener for F {
    fn open(&self, url: &str) -> std::result::Result<Reply, NetError> {
        self(url)
    }
}

/// The real network, following redirects; timeout bounds connecting and each read, not the whole download.
pub struct Web {
    pub timeout: Duration,
}

impl Default for Web {
    fn default() -> Self {
        Web { timeout: REQUEST_TIMEOUT }
    }
}

impl Opener for Web {
    fn open(&self, url: &str) -> std::result::Result<Reply, NetError> {
        let t = Some(self.timeout);
        let config = ureq::Agent::config_builder()
            .timeout_resolve(t).timeout_connect(t).timeout_send_request(t).timeout_recv_response(t).timeout_per_read(t)
            .http_status_as_error(false).user_agent(format!("Telescope/{}", version::version())).build();
        let response = ureq::Agent::new_with_config(config).get(url).call().map_err(|e| {
            let text = e.to_string();
            if text.contains("certificate") || text.contains("UnknownIssuer") { NetError::Certificate } else { NetError::Other(text) }
        })?;
        let status = response.status().as_u16();
        Ok(Reply { status, body: Box::new(response.into_body().into_reader()) })
    }
}

/// What this copy is and where it lives, for the checks that depend on it.
pub struct Updates<'a> {
    pub opener: &'a dyn Opener,
    /// The running build (version::build()).
    pub build: i64,
    pub directory: PathBuf,
}

impl Updates<'_> {
    /// The channel's current manifest, or None when the channel has no release yet (HTTP 404).
    pub fn fetch_manifest(&self, channel: &str) -> Result<Option<Manifest>> {
        let reply = match self.opener.open(&manifest_url(channel)) {
            Ok(r) => r,
            Err(NetError::Certificate) => return err("Couldn't check GitHub's certificate, so updates weren't checked. Security software or a network that inspects HTTPS can cause this."),
            Err(NetError::Other(_)) => return err("Couldn't check for updates. Check the internet connection."),
        };
        if reply.status == 404 {
            return Ok(None);
        }
        if reply.status >= 400 {
            return Err(UpdateError(tr("Couldn't check for updates (HTTP {code}).").replace("{code}", &reply.status.to_string())));
        }
        let mut raw = Vec::new();
        if reply.body.take(MAX_MANIFEST_BYTES as u64 + 1).read_to_end(&mut raw).is_err() {
            return err("Couldn't check for updates. Check the internet connection.");
        }
        if raw.len() > MAX_MANIFEST_BYTES {
            return err("The update information couldn't be read.");
        }
        parse_manifest(&raw).map(Some)
    }

    /// Whether manifest is an update to offer: newer, and not the build rolled back for not starting.
    pub fn is_newer(&self, manifest: Option<&Manifest>) -> bool {
        match manifest {
            Some(m) if self.build > 0 && m.build > self.build => update_guard::failed_build(&self.directory) != Some(m.build),
            _ => false,
        }
    }

    /// Whether manifest is the newer build held back because it was rolled back for not starting.
    pub fn was_rolled_back(&self, manifest: Option<&Manifest>) -> bool {
        match manifest {
            Some(m) if self.build > 0 && m.build > self.build => update_guard::failed_build(&self.directory) == Some(m.build),
            _ => false,
        }
    }

    /// Offer the rolled-back build again.
    pub fn forget_rolled_back(&self) {
        if let Err(e) = std::fs::remove_file(self.directory.join(update_guard::FAILED)) {
            if e.kind() != io::ErrorKind::NotFound {
                log::warn!("Couldn't clear the rolled-back marker: {e}");
            }
        }
    }

    /// Downloads and verifies an asset; the file only gets its real name once verified.
    pub fn download(&self, asset: &Asset, dest_dir: &Path, progress: &mut dyn FnMut(u64, u64), cancelled: &dyn Fn() -> bool) -> Result<PathBuf> {
        let final_path = dest_dir.join(&asset.name);
        let partial = dest_dir.join(format!("{}.part", asset.name));
        let failed = || UpdateError(tr("The download failed. Check the internet connection and try again."));
        let fetched = (|| -> Result<u64> {
            std::fs::create_dir_all(dest_dir).map_err(|_| failed())?;
            let mut reply = self.opener.open(&asset.url).map_err(|_| failed())?;
            if reply.status >= 400 {
                return Err(failed());
            }
            let mut file = std::fs::File::create(&partial).map_err(|_| failed())?;
            let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
            let mut buf = vec![0u8; CHUNK];
            let mut done = 0u64;
            loop {
                if cancelled() {
                    return err("Cancelled.");
                }
                let n = match reply.body.read(&mut buf) {
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => return Err(failed()),
                };
                if n == 0 {
                    break;
                }
                done += n as u64;
                if done > asset.size {
                    return err("The download is bigger than it should be.");
                }
                digest.update(&buf[..n]);
                file.write_all(&buf[..n]).map_err(|_| failed())?;
                progress(done, asset.size);
            }
            if done < asset.size {
                return err("The download was cut short. Check the internet connection and try again.");
            }
            let hex: String = digest.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
            if hex != asset.sha256 {
                return err("The download didn't match its checksum, so it wasn't installed.");
            }
            Ok(done)
        })();
        if let Err(e) = fetched {
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
        std::fs::rename(&partial, &final_path).map_err(|_| {
            let _ = std::fs::remove_file(&partial);
            failed()
        })?;
        Ok(final_path)
    }

    /// Retries once with a newer manifest if a rolling release changed before its download.
    pub fn download_update(&self, manifest: &Manifest, asset: &Asset, dest_dir: &Path, progress: &mut dyn FnMut(u64, u64),
                           cancelled: &dyn Fn() -> bool) -> Result<(Manifest, PathBuf)> {
        let first = match self.download(asset, dest_dir, progress, cancelled) {
            Ok(path) => return Ok((manifest.clone(), path)),
            Err(e) => e,
        };
        if cancelled() {
            return Err(first);
        }
        let latest = self.fetch_manifest(&manifest.channel).ok().flatten();
        let Some(latest) = latest.filter(|l| l.channel == manifest.channel && l.build > manifest.build && !self.was_rolled_back(Some(l))) else {
            return Err(first);
        };
        let Some(replacement) = latest.assets.get(&asset.name).cloned() else {
            return Err(first);
        };
        if cancelled() {
            return err("Cancelled.");
        }
        let path = self.download(&replacement, dest_dir, progress, cancelled)?;
        Ok((latest, path))
    }
}

pub fn platform_asset(manifest: &Manifest) -> Option<&Asset> {
    manifest.assets.get(if cfg!(windows) { WINDOWS_ASSET } else { LINUX_ASSET })
}

pub fn setup_asset(manifest: &Manifest) -> Option<&Asset> {
    manifest.assets.get(WINDOWS_SETUP_ASSET)
}

/// Can this folder take a new file (os.access(W_OK) in Python)?
fn writable(directory: &Path) -> bool {
    let probe = directory.join(format!(".write-check-{}", std::process::id()));
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Why this copy can't replace itself (the UI then offers the release page), or None.
pub fn self_update_blocker(directory: &Path, channel: &str) -> Option<String> {
    if channel == "dev" {
        return Some(tr("This is a source checkout. Update it with git."));
    }
    if !writable(directory) {
        return Some(tr("Telescope's folder isn't writable, so it can't update itself."));
    }
    None
}

// ── Install ───────────────────────────────────────────────────────────────────

fn unexpected_paths<T>() -> Result<T> {
    err("The update contains unexpected paths, so it wasn't installed.")
}

/// A member name that stays inside the folder it's unpacked to.
fn stays_inside(name: &Path) -> bool {
    let mut depth = 0i32;
    for part in name.components() {
        match part {
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

pub fn extract(archive: &Path, staging: &Path) -> Result<()> {
    let unpack_failed = || UpdateError(tr("The download couldn't be unpacked."));
    if staging.exists() {
        std::fs::remove_dir_all(staging).map_err(|_| unpack_failed())?;
    }
    std::fs::create_dir_all(staging).map_err(|_| unpack_failed())?;
    if archive.extension().is_some_and(|e| e == "zip") {
        let file = std::fs::File::open(archive).map_err(|_| unpack_failed())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|_| unpack_failed())?;
        for i in 0..zip.len() {
            let entry = zip.by_index_raw(i).map_err(|_| unpack_failed())?;
            if !stays_inside(Path::new(&*entry.name().map_err(|_| unpack_failed())?)) {
                return unexpected_paths();
            }
        }
        zip.extract(staging).map_err(|_| unpack_failed())
    } else {
        let open = || -> Result<tar::Archive<flate2::read::GzDecoder<std::fs::File>>> {
            let file = std::fs::File::open(archive).map_err(|_| unpack_failed())?;
            Ok(tar::Archive::new(flate2::read::GzDecoder::new(file)))
        };
        let mut checking = open()?;
        for entry in checking.entries().map_err(|_| unpack_failed())? {
            let entry = entry.map_err(|_| unpack_failed())?;
            let path = entry.path().map_err(|_| unpack_failed())?.into_owned();
            if !stays_inside(&path) {
                return unexpected_paths();
            }
            let kind = entry.header().entry_type();
            if !(kind.is_file() || kind.is_dir()) {
                return err("The update contains unexpected files, so it wasn't installed.");
            }
        }
        let mut unpacking = open()?;
        for entry in unpacking.entries().map_err(|_| unpack_failed())? {
            let mut entry = entry.map_err(|_| unpack_failed())?;
            entry.set_preserve_mtime(true);
            entry.unpack_in(staging).map_err(|_| unpack_failed())?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct InstallResult {
    /// The argv that starts the new version.
    pub relaunch: Vec<String>,
    /// Files in use that kept their old version.
    pub skipped: Vec<String>,
}

/// io::Error's text without Rust's "(os error N)", like Python's strerror.
fn strerror(e: &io::Error) -> String {
    let text = e.to_string();
    match text.rfind(" (os error ") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

fn merged(journal: &Value, changes: Value) -> Value {
    let mut out = journal.clone();
    if let (Some(map), Value::Object(extra)) = (out.as_object_mut(), changes) {
        map.extend(extra);
    }
    out
}

/// Journals the swap, then does it; a failure puts the old version back.
fn swap(directory: &Path, journal: Value, mv: Mover) -> Result<Vec<String>> {
    let journaled = update_guard::write_journal(directory, &merged(&journal, json!({"state": "swapping", "from_build": version::build()})));
    if let Err(e) = journaled {
        return Err(UpdateError(tr("Couldn't replace Telescope's files: {error}").replace("{error}", &strerror(&e))));
    }
    let mut skipped = Vec::new();
    if let Err(e) = update_guard::finish_swap(directory, &journal, &mut skipped, mv) {
        if let Err(back) = update_guard::roll_back(directory, &journal, false, &mut |a, b| std::fs::rename(a, b)) {
            log::error!("Putting the old version back failed; the next start tries again: {back}");
        }
        return Err(UpdateError(tr("Couldn't replace Telescope's files: {error}").replace("{error}", &strerror(&e))));
    }
    // The old version stays until the new one has started once (update_guard::confirm)
    if let Err(e) = update_guard::write_journal(directory, &merged(&journal, json!({"state": "trial", "started": false}))) {
        log::warn!("Couldn't mark the update as on trial: {e}");
    }
    Ok(skipped)
}

pub fn install_windows(archive: &Path, directory: &Path, build: i64, mv: Mover) -> Result<InstallResult> {
    let staging = directory.join(STAGING_DIR);
    extract(archive, &staging)?;
    if !staging.join(EXE_NAME).is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        return err("The download doesn't contain Telescope, so it wasn't installed.");
    }
    // A roll-back puts back whatever old exe it finds, so one left from an earlier update must go first
    for leftover in [OLD_EXE_NAME, FAILED_EXE_NAME] {
        match std::fs::remove_file(directory.join(leftover)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(UpdateError(tr("{file} is in use, so the update wasn't installed. Try again after a restart.").replace("{file}", leftover)));
            }
            _ => {}
        }
    }
    let skipped = swap(directory, json!({"platform": "windows", "to_build": build}), mv)?;
    Ok(InstallResult { relaunch: vec![directory.join(EXE_NAME).to_string_lossy().into_owned(), "--after-update".into()], skipped })
}

/// What a Linux download must hold: this app's binary, or the Python app's main.py and package.
fn holds_telescope(entries: &[String]) -> bool {
    let has = |n: &str| entries.iter().any(|e| e == n);
    has("telescope-rs") || (has("main.py") && has("telescope"))
}

pub fn install_linux(archive: &Path, directory: &Path, build: i64, mv: Mover) -> Result<InstallResult> {
    let staging = directory.join(STAGING_DIR);
    extract(archive, &staging)?;
    let mut entries: Vec<String> = std::fs::read_dir(&staging)
        .map(|list| list.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned())).collect())
        .unwrap_or_default();
    entries.sort();
    if !holds_telescope(&entries) {
        let _ = std::fs::remove_dir_all(&staging);
        return err("The download doesn't contain Telescope, so it wasn't installed.");
    }
    let previous = directory.join(PREVIOUS_DIR);
    // A stale copy left in place would be what a roll-back restores, so the update stops here (shutil.rmtree raising)
    if previous.exists() && update_guard::remove(&previous).is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        return err("The update failed.");
    }
    let added: Vec<&String> = entries.iter().filter(|n| !directory.join(n).exists()).collect();
    swap(directory, json!({"platform": "linux", "to_build": build, "entries": entries, "added": added}), mv)?;
    let start = directory.join("start.sh");
    let program = if start.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&start, std::fs::Permissions::from_mode(0o755));
        }
        start
    } else {
        directory.join("telescope-rs")
    };
    Ok(InstallResult { relaunch: vec![program.to_string_lossy().into_owned(), "--after-update".into()], skipped: Vec::new() })
}

/// build: the one being installed, so a roll-back knows not to offer it again.
pub fn install(archive: &Path, directory: &Path, build: i64) -> Result<InstallResult> {
    let mv: Mover = &mut |a, b| std::fs::rename(a, b);
    if cfg!(windows) { install_windows(archive, directory, build, mv) } else { install_linux(archive, directory, build, mv) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("telescope-updates-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest_json(build: i64, channel: &str, assets: Option<Value>) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "schema": 1, "version": "0.6.0", "build": build, "channel": channel,
            "versionName": format!("0.6.0-{channel}.{build}"), "commit": "abc", "protocol": 2,
            "notes": "https://example.invalid/notes",
            "assets": assets.unwrap_or_else(|| json!([{"name": LINUX_ASSET, "url": "https://h/x.tar.gz", "sha256": "a".repeat(64), "size": 1}])),
        })).unwrap()
    }

    /// A status and a body.
    type Canned = (u16, Vec<u8>);

    /// Serves bodies by URL and remembers what was asked.
    struct Fake {
        bodies: Mutex<HashMap<String, Vec<Canned>>>,
        seen: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(pairs: &[(&str, u16, &[u8])]) -> Fake {
            let mut bodies: HashMap<String, Vec<Canned>> = HashMap::new();
            for (url, status, body) in pairs {
                bodies.entry(url.to_string()).or_default().push((*status, body.to_vec()));
            }
            Fake { bodies: Mutex::new(bodies), seen: Mutex::new(Vec::new()) }
        }
        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Opener for Fake {
        fn open(&self, url: &str) -> std::result::Result<Reply, NetError> {
            self.seen.lock().unwrap().push(url.to_string());
            let mut bodies = self.bodies.lock().unwrap();
            let list = bodies.get_mut(url).ok_or_else(|| NetError::Other("down".into()))?;
            let (status, body) = if list.len() > 1 { list.remove(0) } else { list[0].clone() };
            Ok(Reply { status, body: Box::new(io::Cursor::new(body)) })
        }
    }

    fn updates<'a>(opener: &'a dyn Opener, dir: &Path) -> Updates<'a> {
        Updates { opener, build: 100, directory: dir.to_path_buf() }
    }

    fn asset_for(payload: &[u8], size: Option<u64>) -> Asset {
        let hex: String = ring::digest::digest(&ring::digest::SHA256, payload).as_ref().iter().map(|b| format!("{b:02x}")).collect();
        Asset { name: LINUX_ASSET.into(), url: format!("https://h/{LINUX_ASSET}"), sha256: hex, size: size.unwrap_or(payload.len() as u64) }
    }

    fn manifest(build: i64, channel: &str, assets: &[&Asset]) -> Manifest {
        Manifest { version: "1".into(), build, channel: channel.into(), version_name: String::new(), notes: String::new(), protocol: 2,
                   assets: assets.iter().map(|a| (a.name.clone(), (*a).clone())).collect() }
    }

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn rename() -> impl FnMut(&Path, &Path) -> io::Result<()> {
        |a: &Path, b: &Path| std::fs::rename(a, b)
    }

    #[test]
    fn parses_a_manifest_and_names_its_version() {
        let m = parse_manifest(&manifest_json(120, "nightly", None)).unwrap();
        assert_eq!(m.build, 120);
        assert_eq!(m.display_version(), "0.6.0 nightly 120");
        assert_eq!(m.assets[LINUX_ASSET].size, 1);
        assert_eq!(parse_manifest(&manifest_json(120, "stable", None)).unwrap().display_version(), "0.6.0");
    }

    #[test]
    fn rejects_a_manifest_it_cant_trust() {
        let bad = [
            b"not json".to_vec(),
            b"[]".to_vec(),
            manifest_json(1, "nightly", Some(json!([{"name": "x", "url": "http://insecure/x", "sha256": "a".repeat(64), "size": 1}]))),
            manifest_json(1, "nightly", Some(json!([{"name": "x", "url": "https://h/x", "sha256": "short", "size": 1}]))),
            br#"{"version": "1"}"#.to_vec(),
        ];
        for raw in bad {
            assert!(parse_manifest(&raw).is_err(), "{}", String::from_utf8_lossy(&raw));
        }
    }

    #[test]
    fn fetch_uses_the_channel_url_and_treats_404_as_no_release() {
        let dir = tmp("fetch");
        let body = manifest_json(120, "nightly", None);
        let fake = Fake::new(&[(&manifest_url("nightly"), 200, &body), (&manifest_url("stable"), 404, b"")]);
        let u = updates(&fake, &dir);
        assert_eq!(u.fetch_manifest("nightly").unwrap().unwrap().build, 120);
        assert_eq!(fake.seen(), vec![manifest_url("nightly")]);
        assert!(manifest_url("stable").contains("releases/latest/download"));
        assert_eq!(u.fetch_manifest("stable"), Ok(None));
        let down = Fake::new(&[]);
        assert!(updates(&down, &dir).fetch_manifest("stable").unwrap_err().0.contains("internet"));
    }

    #[test]
    fn newer_compares_build_numbers_and_never_offers_a_dev_build_an_update() {
        let dir = tmp("newer");
        let fake = Fake::new(&[]);
        let m = parse_manifest(&manifest_json(120, "nightly", None)).unwrap();
        let at = |build| Updates { opener: &fake, build, directory: dir.clone() };
        assert!(at(100).is_newer(Some(&m)));
        assert!(!at(120).is_newer(Some(&m)));
        assert!(!at(130).is_newer(Some(&m)));
        assert!(!at(0).is_newer(Some(&m)));
        assert!(!at(100).is_newer(None));
    }

    #[test]
    fn default_channel_follows_the_build() {
        assert_eq!(version::channel(), "dev");
        assert_eq!(default_channel(), "stable");
    }

    #[test]
    fn download_verifies_and_reports_progress() {
        let dir = tmp("download");
        let payload = vec![b'x'; 600_000];
        let asset = asset_for(&payload, None);
        let fake = Fake::new(&[(&asset.url, 200, &payload)]);
        let mut seen = Vec::new();
        let path = updates(&fake, &dir).download(&asset, &dir, &mut |d, _| seen.push(d), &|| false).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), payload);
        assert_eq!(*seen.last().unwrap(), payload.len() as u64);
        assert!(seen.len() > 1);
        assert!(!dir.join(format!("{LINUX_ASSET}.part")).exists());
    }

    #[test]
    fn download_refuses_what_doesnt_match() {
        for (served, payload, size, message) in [(&b"tampered"[..], &b"original"[..], None, "checksum"), (b"much longer than promised", b"short", Some(5), "bigger")] {
            let dir = tmp(&format!("refuse-{message}"));
            let asset = asset_for(payload, size);
            let fake = Fake::new(&[(&asset.url, 200, served)]);
            let e = updates(&fake, &dir).download(&asset, &dir, &mut |_, _| {}, &|| false).unwrap_err();
            assert!(e.0.contains(message), "{e}");
            assert!(listing(&dir).is_empty());
        }
    }

    #[test]
    fn download_can_be_cancelled() {
        let dir = tmp("cancel");
        let asset = asset_for(b"abc", None);
        let fake = Fake::new(&[(&asset.url, 200, b"abc")]);
        let e = updates(&fake, &dir).download(&asset, &dir, &mut |_, _| {}, &|| true).unwrap_err();
        assert!(e.0.contains("Cancelled"));
    }

    #[test]
    fn download_update_recovers_when_a_nightly_is_replaced() {
        for old_payload in [&b"old"[..], b"old build much larger than new", b"older"] {
            let dir = tmp("recover");
            let dest = dir.join("dl");
            let payload = b"newer";
            let (old_asset, new_asset) = (asset_for(old_payload, None), asset_for(payload, None));
            let (old, latest) = (manifest(120, "nightly", &[&old_asset]), manifest(121, "nightly", &[&new_asset]));
            let latest_json = serde_json::to_vec(&json!({"version": "1", "build": 121, "channel": "nightly", "protocol": 2, "notes": "",
                "assets": [{"name": new_asset.name, "url": new_asset.url, "sha256": new_asset.sha256, "size": new_asset.size}]})).unwrap();
            let fake = Fake::new(&[(&old_asset.url, 200, payload), (&manifest_url("nightly"), 200, &latest_json)]);
            let mut latest_named = latest.clone();
            latest_named.version_name = "1".into();
            let (selected, path) = updates(&fake, &dir).download_update(&old, &old_asset, &dest, &mut |_, _| {}, &|| false).unwrap();
            assert_eq!(selected, latest_named);
            assert_eq!(std::fs::read(path).unwrap(), payload);
            assert_eq!(fake.seen().iter().filter(|u| **u == old_asset.url).count(), 2);
            assert_eq!(listing(&dest), vec![new_asset.name.clone()]);
        }
    }

    #[test]
    fn download_update_does_not_accept_an_invalid_replacement() {
        for replacement in ["same", "older", "missing", "channel", "rolled back", "corrupt"] {
            let dir = tmp(&format!("invalid-{}", replacement.replace(' ', "-")));
            let asset = asset_for(b"old", None);
            let build = match replacement { "same" => 120, "older" => 119, _ => 121 };
            let channel = if replacement == "channel" { "stable" } else { "nightly" };
            let assets = if replacement == "missing" { json!([]) } else {
                json!([{"name": asset.name, "url": asset.url, "sha256": asset.sha256, "size": asset.size}])
            };
            let latest_json = serde_json::to_vec(&json!({"version": "1", "build": build, "channel": channel, "assets": assets})).unwrap();
            if replacement == "rolled back" {
                std::fs::write(dir.join(update_guard::FAILED), r#"{"build": 121}"#).unwrap();
            }
            let fake = Fake::new(&[(&asset.url, 200, b"odd"), (&manifest_url("nightly"), 200, &latest_json)]);
            let e = updates(&fake, &dir).download_update(&manifest(120, "nightly", &[&asset]), &asset, &dir.join("dl"), &mut |_, _| {}, &|| false).unwrap_err();
            assert!(e.0.contains("checksum"), "{replacement}: {e}");
            let downloads = fake.seen().iter().filter(|u| **u == asset.url).count();
            assert_eq!(downloads, if replacement == "corrupt" { 2 } else { 1 }, "{replacement}");
        }
    }

    #[test]
    fn download_update_does_not_refresh_after_success_or_cancellation() {
        let dir = tmp("norefresh");
        let asset = asset_for(b"old", None);
        let old = manifest(120, "nightly", &[&asset]);
        let fake = Fake::new(&[(&asset.url, 200, b"old")]);
        let u = updates(&fake, &dir);
        assert_eq!(u.download_update(&old, &asset, &dir, &mut |_, _| {}, &|| false).unwrap().0, old);
        assert!(u.download_update(&old, &asset, &dir, &mut |_, _| {}, &|| true).unwrap_err().0.contains("Cancelled"));
        assert!(!fake.seen().contains(&manifest_url("nightly")));
    }

    /// A server that behaves like a network without internet behind it.
    fn offline_url(behaviour: &'static str) -> String {
        use std::net::TcpListener;
        match behaviour {
            "refused" => {
                let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
                return format!("http://127.0.0.1:{port}/x");
            }
            "no dns" => return "http://telescope-update-test.invalid/x".into(),
            _ => {}
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut conn, _)) = listener.accept() else { return };
            let mut buf = [0u8; 4096];
            let head = b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n{";
            match behaviour {
                "no answer" => std::thread::sleep(Duration::from_secs(4)),
                "stalls mid body" => {
                    let _ = conn.read(&mut buf);
                    let _ = conn.write_all(head);
                    std::thread::sleep(Duration::from_secs(4));
                }
                "cut off" => {
                    let _ = conn.read(&mut buf);
                    let _ = conn.write_all(head);
                }
                "not http" => {
                    let _ = conn.read(&mut buf);
                    let _ = conn.write_all(b"hello\r\n\r\n");
                }
                "reset" => {
                    let _ = conn.read(&mut buf);
                    socket_linger_zero(&conn);
                }
                _ => {
                    let _ = conn.read(&mut buf);
                    let body = b"<html>Log in to use this wifi</html>";
                    let _ = conn.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
                    let _ = conn.write_all(body);
                }
            }
        });
        format!("http://127.0.0.1:{port}/x")
    }

    #[cfg(unix)]
    fn socket_linger_zero(conn: &std::net::TcpStream) {
        use std::os::fd::AsRawFd;
        let linger = libc::linger { l_onoff: 1, l_linger: 0 };
        unsafe {
            libc::setsockopt(conn.as_raw_fd(), libc::SOL_SOCKET, libc::SO_LINGER, &linger as *const _ as *const libc::c_void,
                             std::mem::size_of::<libc::linger>() as libc::socklen_t);
        }
    }

    #[cfg(not(unix))]
    fn socket_linger_zero(_conn: &std::net::TcpStream) {}

    const OFFLINE: [&str; 8] = ["refused", "no dns", "no answer", "stalls mid body", "cut off", "not http", "reset", "login page"];

    /// Web at a short timeout, sent wherever it's asked to the offline server.
    struct Redirected {
        url: String,
        web: Web,
    }

    impl Opener for Redirected {
        fn open(&self, _url: &str) -> std::result::Result<Reply, NetError> {
            self.web.open(&self.url)
        }
    }

    #[test]
    fn a_check_without_internet_fails_with_a_message() {
        let dir = tmp("offline-check");
        for behaviour in OFFLINE {
            let opener = Redirected { url: offline_url(behaviour), web: Web { timeout: Duration::from_millis(500) } };
            let started = std::time::Instant::now();
            assert!(updates(&opener, &dir).fetch_manifest("stable").is_err(), "{behaviour}");
            assert!(started.elapsed() < Duration::from_secs(3), "{behaviour}");
        }
    }

    #[test]
    fn a_download_without_internet_fails_and_leaves_nothing() {
        for behaviour in OFFLINE {
            let dir = tmp(&format!("offline-dl-{}", behaviour.replace(' ', "-")));
            let opener = Redirected { url: offline_url(behaviour), web: Web { timeout: Duration::from_millis(500) } };
            let started = std::time::Instant::now();
            let e = updates(&opener, &dir).download(&asset_for(&[b'z'; 1000], None), &dir, &mut |_, _| {}, &|| false).unwrap_err();
            assert!(started.elapsed() < Duration::from_secs(3), "{behaviour}");
            assert!(listing(&dir).is_empty(), "{behaviour}");
            if behaviour != "login page" {
                assert!(e.0.contains("internet connection"), "{behaviour}: {e}");
            }
        }
    }

    fn zip_of(path: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
        path.to_path_buf()
    }

    fn tarball(path: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(path).unwrap(), flate2::Compression::fast());
        let mut t = tar::Builder::new(gz);
        for (name, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            t.append_data(&mut header, name, *data).unwrap();
        }
        t.into_inner().unwrap().finish().unwrap();
        path.to_path_buf()
    }

    #[test]
    fn windows_install_renames_the_running_exe_and_replaces_changed_files() {
        let root = tmp("win");
        let app = root.join("app");
        std::fs::create_dir_all(app.join("unitycapture")).unwrap();
        std::fs::write(app.join(EXE_NAME), "old exe").unwrap();
        std::fs::write(app.join("unitycapture/UnityCaptureFilter64.dll"), "same dll").unwrap();
        std::fs::write(app.join("Telescope.apk"), "old apk").unwrap();
        let archive = zip_of(&root.join(WINDOWS_ASSET), &[(EXE_NAME, b"new exe"), ("unitycapture/UnityCaptureFilter64.dll", b"same dll"), ("Telescope.apk", b"new apk")]);
        let result = install_windows(&archive, &app, 0, &mut rename()).unwrap();
        assert_eq!(std::fs::read_to_string(app.join(EXE_NAME)).unwrap(), "new exe");
        assert_eq!(std::fs::read_to_string(app.join(OLD_EXE_NAME)).unwrap(), "old exe");
        assert_eq!(std::fs::read_to_string(app.join("Telescope.apk")).unwrap(), "new apk");
        assert_eq!(result.relaunch, vec![app.join(EXE_NAME).to_string_lossy().into_owned(), "--after-update".into()]);
        assert!(!app.join(STAGING_DIR).exists());
        assert!(app.join(OLD_EXE_NAME).exists());
        update_guard::confirm(&app);
        update_guard::clean_up_after_update(&app, None);
        assert!(!app.join(OLD_EXE_NAME).exists());
    }

    #[test]
    fn the_first_folder_build_installs_over_the_single_exe() {
        let root = tmp("folder");
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join(EXE_NAME), "onefile exe").unwrap();
        let archive = zip_of(&root.join(WINDOWS_ASSET), &[(EXE_NAME, b"folder exe"), ("lib-130/python311.dll", b"py"), ("lib-130/PyQt6/Qt6/x.dll", b"qt")]);
        install_windows(&archive, &app, 0, &mut rename()).unwrap();
        assert_eq!(std::fs::read_to_string(app.join(EXE_NAME)).unwrap(), "folder exe");
        assert_eq!(std::fs::read_to_string(app.join("lib-130/PyQt6/Qt6/x.dll")).unwrap(), "qt");
    }

    #[test]
    fn a_folder_build_update_adds_its_own_libs_and_the_old_ones_wait_for_the_confirm() {
        let root = tmp("libs");
        let app = root.join("app");
        std::fs::create_dir_all(app.join("lib-130")).unwrap();
        std::fs::write(app.join("lib-130/python311.dll"), "old py").unwrap();
        std::fs::write(app.join(EXE_NAME), "exe 130").unwrap();
        std::fs::create_dir_all(app.join("platform-tools")).unwrap();
        std::fs::write(app.join("platform-tools/adb.exe"), "adb").unwrap();
        let archive = zip_of(&root.join(WINDOWS_ASSET), &[(EXE_NAME, b"exe 131"), ("lib-131/python311.dll", b"new py"), ("platform-tools/adb.exe", b"adb")]);
        let result = install_windows(&archive, &app, 0, &mut rename()).unwrap();
        assert!(result.skipped.is_empty());
        assert_eq!(std::fs::read_to_string(app.join("lib-130/python311.dll")).unwrap(), "old py");
        assert_eq!(std::fs::read_to_string(app.join("lib-131/python311.dll")).unwrap(), "new py");
        update_guard::clean_up_after_update(&app, None);
        assert!(app.join("lib-130").exists());
        assert!(app.join("platform-tools/adb.exe").exists());
    }

    #[test]
    fn the_new_lib_folder_is_in_place_before_the_exe_is_swapped() {
        let root = tmp("order");
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join(EXE_NAME), "exe 130").unwrap();
        let archive = zip_of(&root.join(WINDOWS_ASSET), &[(EXE_NAME, b"exe 131"), ("lib-131/a.dll", b"a"), ("lib-131/sub/b.dll", b"b")]);
        let mut seen = Vec::new();
        let lib = app.join("lib-131");
        let mut mv = |a: &Path, b: &Path| {
            if b.ends_with(EXE_NAME) && std::fs::read_to_string(a).is_ok_and(|t| t.contains("131")) {
                let mut found: Vec<String> = Vec::new();
                fn walk(d: &Path, out: &mut Vec<String>) {
                    for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                        out.push(e.file_name().to_string_lossy().into_owned());
                        walk(&e.path(), out);
                    }
                }
                walk(&lib, &mut found);
                found.sort();
                seen.push(found);
            }
            std::fs::rename(a, b)
        };
        install_windows(&archive, &app, 0, &mut mv).unwrap();
        assert_eq!(seen, vec![vec!["a.dll".to_string(), "b.dll".into(), "sub".into()]]);
    }

    #[test]
    fn windows_install_refuses_an_archive_without_the_app() {
        let root = tmp("noapp");
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join(EXE_NAME), "old exe").unwrap();
        let e = install_windows(&zip_of(&root.join("w.zip"), &[("readme.txt", b"hi")]), &app, 0, &mut rename()).unwrap_err();
        assert!(e.0.contains("doesn't contain"));
        assert_eq!(std::fs::read_to_string(app.join(EXE_NAME)).unwrap(), "old exe");
    }

    #[test]
    fn an_archive_escaping_its_folder_is_refused() {
        let root = tmp("escape");
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        let e = install_windows(&zip_of(&root.join("w.zip"), &[("../evil.exe", b"x")]), &app, 0, &mut rename()).unwrap_err();
        assert!(e.0.contains("unexpected"));
        assert!(!root.join("evil.exe").exists());
    }

    fn linux_app(root: &Path) -> PathBuf {
        let app = root.join("app");
        std::fs::create_dir_all(app.join("telescope")).unwrap();
        std::fs::write(app.join("main.py"), "old").unwrap();
        std::fs::write(app.join("telescope/app.py"), "old").unwrap();
        std::fs::write(app.join("telescope/gone.py"), "removed in the new version").unwrap();
        std::fs::write(app.join("start.sh"), "old").unwrap();
        std::fs::write(app.join("notes-the-user-kept.txt"), "mine").unwrap();
        app
    }

    const NEW_LINUX: [(&str, &[u8]); 4] = [("main.py", b"new"), ("telescope/app.py", b"new"), ("start.sh", b"#!/bin/sh\n"), ("requirements.txt", b"x")];

    #[test]
    fn linux_install_swaps_the_app_and_keeps_the_old_one() {
        let root = tmp("linux");
        let app = linux_app(&root);
        let result = install_linux(&tarball(&root.join("l.tar.gz"), &NEW_LINUX), &app, 0, &mut rename()).unwrap();
        assert_eq!(std::fs::read_to_string(app.join("main.py")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(app.join("telescope/app.py")).unwrap(), "new");
        assert!(!app.join("telescope/gone.py").exists());
        assert_eq!(std::fs::read_to_string(app.join("notes-the-user-kept.txt")).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(app.join(".previous/main.py")).unwrap(), "old");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(app.join("start.sh")).unwrap().permissions().mode() & 0o111, 0o111);
        }
        assert_eq!(result.relaunch, vec![app.join("start.sh").to_string_lossy().into_owned(), "--after-update".into()]);
    }

    #[test]
    fn linux_install_rolls_back_when_a_swap_fails() {
        let root = tmp("linux-fail");
        let app = linux_app(&root);
        let mut flaky = |a: &Path, b: &Path| {
            if b.ends_with("start.sh") && a.to_string_lossy().contains(STAGING_DIR) {
                return Err(io::Error::from_raw_os_error(13));
            }
            std::fs::rename(a, b)
        };
        let e = install_linux(&tarball(&root.join("l.tar.gz"), &NEW_LINUX), &app, 0, &mut flaky).unwrap_err();
        assert!(e.0.contains("Permission denied") && !e.0.contains("os error"), "{e}");
        assert_eq!(std::fs::read_to_string(app.join("main.py")).unwrap(), "old");
        assert!(app.join("telescope/gone.py").exists());
        assert_eq!(std::fs::read_to_string(app.join("start.sh")).unwrap(), "old");
        assert!(!app.join("requirements.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn linux_install_stops_when_an_old_previous_folder_cannot_be_removed() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tmp("linux-stale-previous");
        if std::fs::metadata(&root).unwrap().uid() == 0 {
            return; // root removes anything
        }
        let app = linux_app(&root);
        let stuck = app.join(".previous/stuck");
        std::fs::create_dir_all(&stuck).unwrap();
        std::fs::write(stuck.join("main.py"), "a stale copy").unwrap();
        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = install_linux(&tarball(&root.join("l.tar.gz"), &NEW_LINUX), &app, 0, &mut rename());
        std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Carrying on would back up nothing new and a roll-back would restore the stale copy
        assert!(result.is_err(), "installed over a stale .previous");
        assert_eq!(std::fs::read_to_string(app.join("main.py")).unwrap(), "old");
        assert!(update_guard::read_journal(&app).is_none());
        assert!(!app.join(STAGING_DIR).exists());
    }

    #[test]
    fn linux_install_takes_this_apps_own_build() {
        let root = tmp("linux-rs");
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("telescope-rs"), "old").unwrap();
        let result = install_linux(&tarball(&root.join("l.tar.gz"), &[("telescope-rs", b"new")]), &app, 0, &mut rename()).unwrap();
        assert_eq!(std::fs::read_to_string(app.join("telescope-rs")).unwrap(), "new");
        assert_eq!(result.relaunch[0], app.join("telescope-rs").to_string_lossy());
        let e = install_linux(&tarball(&root.join("x.tar.gz"), &[("readme", b"hi")]), &app, 0, &mut rename()).unwrap_err();
        assert!(e.0.contains("doesn't contain"));
    }

    #[test]
    fn a_source_checkout_or_read_only_folder_only_gets_a_link() {
        let root = tmp("blocker");
        let app = root.join("app");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        assert_eq!(self_update_blocker(&app, "nightly"), None);
        assert!(self_update_blocker(&root.join("missing"), "nightly").unwrap().contains("writable"));
        assert!(self_update_blocker(&app, "dev").unwrap().contains("source checkout"));
    }

    #[test]
    fn a_rolled_back_build_is_named_and_can_be_tried_again() {
        let dir = tmp("rolled");
        std::fs::write(dir.join(".update-failed"), r#"{"build": 200}"#).unwrap();
        let fake = Fake::new(&[]);
        let at = |build| Updates { opener: &fake, build, directory: dir.clone() };
        let m = manifest(200, "nightly", &[]);
        assert!(at(150).was_rolled_back(Some(&m)));
        assert!(!at(150).is_newer(Some(&m)));
        assert!(!at(150).was_rolled_back(Some(&manifest(201, "nightly", &[]))));
        assert!(!at(200).was_rolled_back(Some(&m)));
        at(150).forget_rolled_back();
        assert!(at(150).is_newer(Some(&m)));
    }
}
