//! The vision pack (vision_pack.py): MediaPipe and its models as one download from the release, checked and unpacked. It lands
//! in the same folder the Python app uses, one folder per pack id, so a pack either app downloaded works for both.

use ring::digest::{Context, SHA256};
use serde_json::Value;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use telescope_core::i18n::tr;

pub const SEGMENTER_MODEL: &str = "selfie_segmenter_landscape.tflite";
pub const POSE_MODEL: &str = "pose_landmarker_lite.task";
/// Written last, holding the pack's id: a folder without it is a download that didn't finish.
pub const DONE: &str = ".verified";
const TIMEOUT: Duration = Duration::from_secs(30);
/// Zips of the same files can come out a little bigger on another machine's zlib.
const SLACK: f64 = 1.1;
const CHUNK: usize = 256 * 1024;
const REPO: &str = "LunarKittyy/Telescope";
/// What scripts/build_vision_pack.py --write recorded for this version.
pub const PACK_INFO: &str = include_str!("../../../../desktop/telescope/vision_pack.json");

/// Why the pack couldn't be downloaded or unpacked, said for the person who clicked Download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackError(pub String);

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where vision packs are unpacked, one folder per pack (vision_pack.default_dir).
pub fn default_dir() -> PathBuf {
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if let Some(dev) = env(telescope_core::config::DEV_PROFILE_ENV) {
        return PathBuf::from(dev).join("vision");
    }
    let home = || env("HOME").or_else(|| env("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    if cfg!(windows) {
        let base = env("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(home);
        return base.join("Telescope").join("vision");
    }
    let base = env("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local").join("share"));
    base.join("telescope").join("vision")
}

/// Which pack this computer needs, or None when there isn't one for it (only 64-bit Intel/AMD for now).
pub fn platform_key() -> Option<&'static str> {
    if std::env::consts::ARCH != "x86_64" {
        return None;
    }
    if cfg!(windows) { Some("windows") } else if cfg!(target_os = "linux") { Some("linux") } else { None }
}

/// This platform's entry in vision_pack.json.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackInfo {
    pub key: Option<String>,
    /// The full sha256 of the pack's files (tree_hash), None without an entry.
    pub id: Option<String>,
    pub size: u64,
}

impl PackInfo {
    /// The entry this build ships with, for this computer.
    pub fn current() -> PackInfo {
        let info: Value = serde_json::from_str(PACK_INFO).unwrap_or(Value::Null);
        PackInfo::from_json(&info, platform_key())
    }

    pub fn from_json(info: &Value, key: Option<&str>) -> PackInfo {
        let entry = key.and_then(|k| info.get(k)).filter(|e| e.get("id").is_some_and(Value::is_string));
        PackInfo {
            key: key.map(str::to_string),
            id: entry.and_then(|e| e["id"].as_str()).map(str::to_string),
            size: entry.and_then(|e| e.get("size")).and_then(Value::as_u64).unwrap_or(0),
        }
    }

    /// The folder name: the id's first 16 characters.
    pub fn pack_id(&self) -> Option<String> {
        self.id.as_ref().map(|id| id.chars().take(16).collect())
    }

    pub fn asset_name(&self) -> Option<String> {
        Some(format!("Telescope-vision-{}-{}.zip", self.key.as_ref()?, self.pack_id()?))
    }

    pub fn download_size(&self) -> u64 {
        if self.id.is_some() { self.size } else { 0 }
    }

    /// This version's pack is unpacked and checked (cheap: nothing is loaded).
    pub fn installed(&self, vision_dir: &Path) -> bool {
        let Some(pid) = self.pack_id() else { return false };
        std::fs::read_to_string(vision_dir.join(&pid).join(DONE)).is_ok_and(|t| t.trim() == pid)
    }

    /// The pack this version uses: site/ holds MediaPipe, models/ the two models.
    pub fn folder(&self, vision_dir: &Path) -> PathBuf {
        vision_dir.join(self.pack_id().unwrap_or_else(|| "none".into()))
    }
}

/// Download link for a file from the release this build belongs to (dev builds use nightly), as version.release_asset_url.
pub fn release_asset_url(name: &str) -> String {
    let stable = option_env!("TELESCOPE_CHANNEL") == Some("stable");
    let tag = if stable { format!("v{}", telescope_core::version::version()) } else { "nightly".into() };
    format!("https://github.com/{REPO}/releases/download/{tag}/{name}")
}

fn sha256_hex(data: &[u8]) -> String {
    let mut c = Context::new(&SHA256);
    c.update(data);
    hex(c.finish().as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn files_under(folder: &Path, rel: &mut Vec<String>, out: &mut Vec<(Vec<String>, PathBuf)>) -> io::Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        rel.push(entry.file_name().to_string_lossy().into_owned());
        let meta = std::fs::metadata(&path)?;
        if meta.is_dir() {
            files_under(&path, rel, out)?;
        } else if meta.is_file() {
            out.push((rel.clone(), path));
        }
        rel.pop();
    }
    Ok(())
}

/// What's in a folder, as one sha256 over each file's path and sha256 (the zip itself can differ by zlib), in case-sensitive
/// path order on every platform.
pub fn tree_hash(folder: &Path) -> io::Result<String> {
    let mut files = Vec::new();
    files_under(folder, &mut Vec::new(), &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut lines = String::new();
    for (parts, path) in files {
        let rel = parts.join("/");
        if rel == DONE {
            continue;
        }
        let mut digest = Context::new(&SHA256);
        let mut f = std::fs::File::open(&path)?;
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            digest.update(&buf[..n]);
        }
        lines.push_str(&format!("{rel}\0{}\n", hex(digest.finish().as_ref())));
    }
    Ok(sha256_hex(lines.as_bytes()))
}

/// Opens a URL for reading (telescope.web.urlopen with a timeout).
pub type Opener<'a> = &'a dyn Fn(&str, Duration) -> io::Result<Box<dyn Read>>;

/// The real web, through the system's certificate store.
pub fn web(url: &str, timeout: Duration) -> io::Result<Box<dyn Read>> {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .https_only(true)
        .timeout_global(Some(timeout))
        .user_agent(format!("Telescope/{}", telescope_core::version::version()))
        .build()
        .into();
    let response = agent.get(url).call().map_err(io::Error::other)?;
    Ok(Box::new(response.into_body().into_reader()))
}

enum Failed {
    Pack(PackError),
    Io(io::Error),
}

impl From<io::Error> for Failed {
    fn from(e: io::Error) -> Failed {
        Failed::Io(e)
    }
}

/// Download this version's pack, check it and unpack it; older packs go.
pub fn download(info: &PackInfo, vision_dir: &Path, progress: &mut dyn FnMut(u64, u64), cancelled: &dyn Fn() -> bool,
                opener: Opener, url: Option<&str>) -> Result<PathBuf, PackError> {
    let (Some(id), Some(name), Some(pid)) = (info.id.clone(), info.asset_name(), info.pack_id()) else {
        return Err(PackError(tr("There's no download for this computer yet.")));
    };
    let url = url.map(str::to_string).unwrap_or_else(|| release_asset_url(&name));
    let size = info.size;
    let archive = vision_dir.join(format!(".{pid}.zip.part"));
    let staging = vision_dir.join(format!(".{pid}.unpacking"));
    let mut run = || -> Result<PathBuf, Failed> {
        std::fs::create_dir_all(vision_dir)?;
        let mut r = opener(&url, TIMEOUT)?;
        let mut f = std::fs::File::create(&archive)?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        loop {
            if cancelled() {
                return Err(Failed::Pack(PackError(tr("Cancelled."))));
            }
            let n = read_chunk(&mut *r, &mut buf)?;
            if n == 0 {
                break;
            }
            done += n as u64;
            if size > 0 && done as f64 > size as f64 * SLACK {
                return Err(Failed::Pack(PackError(tr("The download is bigger than it should be."))));
            }
            f.write_all(&buf[..n])?;
            progress(done, size);
        }
        drop(f);
        unpack(&archive, &staging)?;
        if tree_hash(&staging)? != id {
            return Err(Failed::Pack(PackError(tr("The download didn't match what this version of Telescope expects, so it wasn't used."))));
        }
        std::fs::write(staging.join(DONE), &pid)?;
        let last = vision_dir.join(&pid);
        if last.exists() {
            std::fs::remove_dir_all(&last)?;
        }
        std::fs::rename(&staging, &last)?;
        Ok(last)
    };
    let result = run();
    let _ = std::fs::remove_file(&archive);
    let _ = std::fs::remove_dir_all(&staging);
    match result {
        Ok(last) => {
            remove(vision_dir, Some(&pid));
            Ok(last)
        }
        Err(Failed::Pack(e)) => Err(e),
        Err(Failed::Io(e)) => {
            log::warn!("Vision pack download failed: {e}");
            Err(PackError(tr("The download failed. Check the internet connection and try again.")))
        }
    }
}

/// Up to a chunk's worth (urllib's read(n) gives up to n bytes, fewer only at the end).
fn read_chunk(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

/// A zip entry's path inside the staging folder, or None when it would land outside it (or on it).
fn inside(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in Path::new(name).components() {
        match part {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn unpack(archive: &Path, staging: &Path) -> Result<(), Failed> {
    let _ = std::fs::remove_dir_all(staging);
    std::fs::create_dir_all(staging)?;
    let broken = |_| Failed::Pack(PackError(tr("The download couldn't be unpacked.")));
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?).map_err(broken)?;
    let mut targets = Vec::new();
    for i in 0..zip.len() {
        let name = zip.by_index_raw(i).map_err(broken)?.name().map_err(broken)?.to_string();
        let Some(rel) = inside(&name) else {
            return Err(Failed::Pack(PackError(tr("The download contains unexpected paths, so it wasn't used."))));
        };
        targets.push((rel, name.ends_with('/')));
    }
    for (i, (rel, dir)) in targets.into_iter().enumerate() {
        let target = staging.join(rel);
        if dir {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut entry = zip.by_index(i).map_err(broken)?;
        let mut out = std::fs::File::create(&target)?;
        io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

/// Delete the packs (and leftovers of unfinished downloads), all but keep.
pub fn remove(vision_dir: &Path, keep: Option<&str>) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(vision_dir) else { return Ok(()) };
    let mut failed = Ok(());
    for entry in entries.flatten() {
        if keep.is_some_and(|k| entry.file_name().to_string_lossy() == k) {
            continue;
        }
        let path = entry.path();
        let gone = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
        if let Err(e) = gone {
            log::warn!("Couldn't remove {}", path.display());
            failed = Err(e.to_string());
        }
    }
    failed
}

#[cfg(test)]
#[path = "pack_tests.rs"]
pub(crate) mod tests;
