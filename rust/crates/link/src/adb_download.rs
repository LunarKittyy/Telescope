//! Windows: fetch adb from Google when the person asks for it, rather than shipping it (platform/adb_download.py).
//! adb's licence doesn't allow passing it on, so this reads Google's own SDK index for the current Windows
//! platform-tools, downloads that zip, checks its size and SHA-1, and keeps only what adb needs in downloaded_dir().

use crate::util::{hex, random_hex};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use telescope_core::i18n::tr;

pub use crate::adb::downloaded_dir;

pub const REPOSITORY: &str = "https://dl.google.com/android/repository/";
pub const INDEX_URL: &str = "https://dl.google.com/android/repository/repository2-3.xml";
pub const LICENSE_URL: &str = "https://developer.android.com/studio/terms";
/// No-break space: it shows in a wrapped dialog.
pub const APPROX_SIZE: &str = "8\u{a0}MB";
/// What adb.exe needs next to it, plus Google's notices. The rest of platform-tools is left out.
pub const NEEDED: [&str; 6] = ["adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll", "libwinpthread-1.dll", "NOTICE.txt", "source.properties"];
/// The zip is about 8 MB; anything far bigger isn't it.
pub const MAX_ZIP: u64 = 64 << 20;
const MAX_INDEX: u64 = 16 << 20;
const TIMEOUT: Duration = Duration::from_secs(30);
const LEFTOVER_AGE: Duration = Duration::from_secs(3600);

/// Add phone and Advanced each have a Get adb; one download at a time.
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Archive {
    pub url: String,
    pub size: u64,
    pub sha1: String,
    pub revision: String,
}

/// Why a fetch failed, as far as the sentence shown for it cares.
#[derive(Debug)]
pub enum FetchError {
    /// No connection, a timeout, a name that didn't resolve.
    Offline(String),
    /// The server's certificate couldn't be checked (security software or an inspecting network, often).
    Certificate(String),
    /// An HTTP error status.
    Http(u16),
    Other(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Offline(why) | FetchError::Certificate(why) | FetchError::Other(why) => f.write_str(why),
            FetchError::Http(code) => write!(f, "HTTP {code}"),
        }
    }
}

/// Opens a URL for reading: the final URL after redirects, and the body.
pub trait Fetch: Sync {
    fn open(&self, url: &str, timeout: Duration) -> Result<(String, Box<dyn Read + Send>), FetchError>;
}

/// The real web, through the system's certificate store (telescope.web.urlopen), so security software that
/// inspects HTTPS with its own root still works.
pub struct Web;

impl Fetch for Web {
    fn open(&self, url: &str, timeout: Duration) -> Result<(String, Box<dyn Read + Send>), FetchError> {
        let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .https_only(true)
            .timeout_global(Some(timeout))
            .user_agent(concat!("Telescope/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let response = agent.get(url).call().map_err(fetch_error)?;
        let (_, body) = response.into_parts();
        Ok((url.to_string(), Box::new(body.into_reader())))
    }
}

fn is_certificate(e: &rustls::Error) -> bool {
    matches!(e, rustls::Error::InvalidCertificate(_) | rustls::Error::InvalidCertRevocationList(_))
}

fn fetch_error(e: ureq::Error) -> FetchError {
    match e {
        ureq::Error::StatusCode(code) => FetchError::Http(code),
        ureq::Error::Rustls(ref inner) if is_certificate(inner) => FetchError::Certificate(e.to_string()),
        ureq::Error::Io(ref io) if io.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()).is_some_and(is_certificate) => {
            FetchError::Certificate(e.to_string())
        }
        ureq::Error::Io(_) | ureq::Error::Timeout(_) | ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => {
            FetchError::Offline(e.to_string())
        }
        other => FetchError::Other(other.to_string()),
    }
}

pub fn downloaded_adb() -> Option<PathBuf> {
    Some(downloaded_dir().join("adb.exe")).filter(|p| p.is_file())
}

fn child<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn text_of(node: Option<roxmltree::Node>) -> String {
    node.map(|n| n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect::<String>())
        .unwrap_or_default().trim().to_string()
}

fn all_numbers(version: &str) -> bool {
    version.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// The platform-tools zip for host_os in Google's SDK index, or why the index doesn't have one.
pub fn find_archive(index: &[u8], host_os: &str) -> Result<Archive, String> {
    let text = std::str::from_utf8(index).map_err(|e| e.to_string())?;
    let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
    for package in doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "remotePackage") {
        if package.attribute("path") != Some("platform-tools") {
            continue;
        }
        let rev = child(package, "revision");
        let parts: Vec<String> = rev.into_iter()
            .flat_map(|r| ["major", "minor", "micro"].into_iter().filter_map(move |p| child(r, p)))
            .map(|n| text_of(Some(n)))
            .collect();
        let mut revision = parts.join(".");
        // It's shown in the UI as the version
        if !all_numbers(&revision) {
            revision.clear();
        }
        for archive in package.descendants().filter(|n| n.is_element() && n.tag_name().name() == "archive") {
            let (Some(host), Some(complete)) = (child(archive, "host-os"), child(archive, "complete")) else { continue };
            if text_of(Some(host)) != host_os {
                continue;
            }
            let (Some(url), Some(size), Some(checksum)) = (child(complete, "url"), child(complete, "size"), child(complete, "checksum")) else {
                continue;
            };
            if checksum.attribute("type") != Some("sha1") {
                continue;
            }
            let name = text_of(Some(url));
            // Only files next to the index, on Google's host
            if name.contains('/') || name.contains(':') || !name.ends_with(".zip") {
                return Err(format!("unexpected archive name {name:?}"));
            }
            let size = text_of(Some(size)).parse::<u64>().map_err(|e| e.to_string())?;
            return Ok(Archive { url: format!("{REPOSITORY}{name}"), size, sha1: text_of(Some(checksum)).to_lowercase(), revision });
        }
    }
    Err(format!("no platform-tools for {host_os} in Google's index"))
}

/// Why a step failed: a fetch, or something about what came back.
enum Failed {
    Fetch(FetchError),
    Other(String),
}

fn fetch(web: &dyn Fetch, url: &str, out: &mut dyn Write, limit: u64) -> Result<(), Failed> {
    let (final_url, mut body) = web.open(url, TIMEOUT).map_err(Failed::Fetch)?;
    // A redirect off HTTPS: the checksum would come from anyone
    if !final_url.starts_with("https://") {
        return Err(Failed::Other(format!("it was redirected to {final_url}")));
    }
    let mut got = 0u64;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = body.read(&mut buf).map_err(|e| Failed::Fetch(FetchError::Offline(e.to_string())))?;
        if n == 0 {
            return Ok(());
        }
        got += n as u64;
        if got > limit {
            return Err(Failed::Other("it's bigger than expected".into()));
        }
        out.write_all(&buf[..n]).map_err(|e| Failed::Other(e.to_string()))?;
    }
}

/// The version Google's source.properties gives for the downloaded adb, or "" if it doesn't say.
pub fn downloaded_revision(dest: Option<&Path>) -> String {
    let dest = dest.map(Path::to_path_buf).unwrap_or_else(downloaded_dir);
    let Ok(bytes) = std::fs::read(dest.join("source.properties")) else { return String::new() };
    for line in String::from_utf8_lossy(&bytes).lines() {
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        if key.trim() == "Pkg.Revision" {
            let value = value.trim();
            return if all_numbers(value) { value.to_string() } else { String::new() };
        }
    }
    String::new()
}

type Rename<'a> = &'a dyn Fn(&Path, &Path) -> io::Result<()>;

fn rename(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

/// Downloads adb into dest (downloaded_dir()) unless it's already there: Ok(version, or "" if unknown) or Err(why
/// not). Never leaves a half-made folder where adb_exe() looks.
pub fn download_adb(web: &dyn Fetch, dest: Option<&Path>, progress: &dyn Fn(&str)) -> Result<String, String> {
    download_with(web, dest, progress, &rename)
}

fn download_with(web: &dyn Fetch, dest: Option<&Path>, say: &dyn Fn(&str), rename: Rename) -> Result<String, String> {
    let dest = dest.map(Path::to_path_buf).unwrap_or_else(downloaded_dir);
    let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Already there (the other dialog got it, say): its adb may be running, which keeps the folder from moving
    if dest.join("adb.exe").is_file() {
        return Ok(downloaded_revision(Some(&dest)));
    }
    if let Some(parent) = dest.parent() {
        clear_leftovers(parent);
    }
    download(web, &dest, say, rename)
}

/// Work folders a download that was cut short (Telescope killed, say) left behind.
fn clear_leftovers(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(".adb-") {
            continue;
        }
        let old = entry.metadata().and_then(|m| m.modified()).ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|age| age > LEFTOVER_AGE);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// A fresh work folder next to dest, so the final move is a rename on the same drive.
fn work_folder(parent: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(parent)?;
    loop {
        let work = parent.join(format!(".adb-{}", random_hex(6)));
        match std::fs::create_dir(&work) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            other => return other.map(|_| work),
        }
    }
}

/// Removes the work folder however the download ends.
struct Work(PathBuf);

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn download(web: &dyn Fetch, dest: &Path, say: &dyn Fn(&str), rename: Rename) -> Result<String, String> {
    say(&tr("Finding the current adb…"));
    let mut index = Vec::new();
    let archive = fetch(web, INDEX_URL, &mut index, MAX_INDEX).and_then(|()| {
        let archive = find_archive(&index, "windows").map_err(Failed::Other)?;
        if archive.size > MAX_ZIP {
            return Err(Failed::Other(format!("platform-tools is listed at {} bytes", archive.size)));
        }
        Ok(archive)
    });
    let archive = match archive {
        Ok(archive) => archive,
        Err(failed) => {
            log::warn!("adb index: {}", describe(&failed));
            return Err(offline(&failed).unwrap_or_else(|| tr("Google's list of downloads wasn't what Telescope expected. Try again later.")));
        }
    };

    let parent = dest.parent().unwrap_or(Path::new("."));
    let work = Work(work_folder(parent).map_err(|e| {
        log::warn!("adb work folder: {e}");
        tr("Couldn't download adb. Try again.")
    })?);
    let zip_path = work.0.join("platform-tools.zip");
    if archive.revision.is_empty() {
        say(&tr("Downloading adb from Google…"));
    } else {
        say(&tr("Downloading adb {version} from Google…").replace("{version}", &archive.revision));
    }
    let fetched = std::fs::File::create(&zip_path).map_err(|e| Failed::Other(e.to_string()))
        .and_then(|mut file| fetch(web, &archive.url, &mut file, archive.size));
    if let Err(failed) = fetched {
        log::warn!("adb download: {}", describe(&failed));
        return Err(offline(&failed).unwrap_or_else(|| tr("Couldn't download adb. Try again.")));
    }
    let size = std::fs::metadata(&zip_path).map(|m| m.len()).unwrap_or(0);
    if size != archive.size || sha1_of(&zip_path).as_deref() != Some(archive.sha1.as_str()) {
        return Err(tr("The download didn't match Google's checksum, so it wasn't used. Try again."));
    }
    say(&tr("Unpacking…"));
    let unpacked = work.0.join("platform-tools");
    if let Err(e) = unzip(&zip_path, &unpacked) {
        log::warn!("adb unpack: {e}");
        return Err(tr("Couldn't unpack adb. Try again."));
    }
    if !unpacked.join("adb.exe").is_file() {
        return Err(tr("Google's download doesn't have adb.exe in it anymore."));
    }
    put_in_place(&unpacked, dest, &work.0, rename)?;
    Ok(archive.revision)
}

fn describe(failed: &Failed) -> String {
    match failed {
        Failed::Fetch(e) => e.to_string(),
        Failed::Other(why) => why.clone(),
    }
}

/// Only what adb needs, from platform-tools/ in the zip (Google's layout) or from its top level.
fn unzip(zip_path: &Path, unpacked: &Path) -> io::Result<()> {
    std::fs::create_dir(unpacked)?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(zip_path)?).map_err(io::Error::other)?;
    for name in NEEDED {
        let inner = format!("platform-tools/{name}");
        let member = [inner.as_str(), name].into_iter().find(|m| zip.index_for_name(m).is_some());
        let Some(member) = member else { continue };
        let mut src = zip.by_name(member).map_err(io::Error::other)?;
        let mut dst = std::fs::File::create(unpacked.join(name))?;
        io::copy(&mut src, &mut dst)?;
    }
    Ok(())
}

fn put_in_place(unpacked: &Path, dest: &Path, work: &Path, rename: Rename) -> Result<(), String> {
    let mut old = None;
    let moved = (|| {
        // A folder without adb.exe in it: one that something else half emptied
        if dest.exists() {
            let to = work.join("old");
            rename(dest, &to)?;
            old = Some(to);
        }
        rename(unpacked, dest)
    })();
    if let Err(e) = moved {
        if let Some(old) = old.filter(|_| !dest.exists()) {
            let _ = rename(&old, dest);
        }
        log::warn!("adb move: {e}");
        return Err(tr("Couldn't put adb in place. Is another program using it? Try again."));
    }
    Ok(())
}

/// Keeps an adb the person got some other way (Google blocks the download in some countries): source is adb.exe,
/// whose folder has the DLLs it needs, or a platform-tools zip. Ok(version or "") or Err(why not).
pub fn use_own_adb(source: &Path, dest: Option<&Path>) -> Result<String, String> {
    let dest = dest.map(Path::to_path_buf).unwrap_or_else(downloaded_dir);
    let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let parent = dest.parent().unwrap_or(Path::new("."));
    let work = Work(work_folder(parent).map_err(|e| {
        log::warn!("own adb: {e}");
        tr("Couldn't copy adb from there.")
    })?);
    clear_leftovers(parent);
    let unpacked = work.0.join("platform-tools");
    let copied = if source.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        unzip(source, &unpacked)
    } else {
        std::fs::create_dir(&unpacked).and_then(|()| {
            let from = source.parent().unwrap_or(Path::new("."));
            for name in NEEDED.iter().filter(|n| from.join(n).is_file()) {
                std::fs::copy(from.join(name), unpacked.join(name))?;
            }
            Ok(())
        })
    };
    if let Err(e) = copied {
        log::warn!("own adb: {e}");
        return Err(tr("Couldn't copy adb from there."));
    }
    if !unpacked.join("adb.exe").is_file() {
        return Err(tr("That doesn't have adb.exe in it."));
    }
    put_in_place(&unpacked, &dest, &work.0, &rename)?;
    Ok(downloaded_revision(Some(&dest)))
}

/// A plain sentence for a network failure, None for anything else.
fn offline(failed: &Failed) -> Option<String> {
    let Failed::Fetch(e) = failed else { return None };
    Some(match e {
        FetchError::Certificate(_) => tr("Couldn't check Google's certificate, so nothing was downloaded. Security software or a network that inspects HTTPS can cause this."),
        FetchError::Offline(_) => tr("Couldn't reach Google. Check the internet connection and try again."),
        FetchError::Http(code @ (403 | 404 | 451)) => tr("Google wouldn't send adb to this computer (it said {code}). Google blocks Android downloads in some countries. If you get platform-tools another way, pick its adb.exe or zip instead.")
            .replace("{code}", &code.to_string()),
        FetchError::Http(code) => tr("Google's server said {code}. Try again later.").replace("{code}", &code.to_string()),
        FetchError::Other(_) => return None,
    })
}

/// What Google's index gives; the index itself comes over HTTPS from Google.
fn sha1_of(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            return Some(hex(ctx.finish().as_ref()));
        }
        ctx.update(&buf[..n]);
    }
}

/// What a download (or keeping an adb picked by hand) runs: progress texts in, Ok(version) or Err(why) out.
pub type DownloadJob = Arc<dyn Fn(&dyn Fn(&str)) -> Result<String, String> + Send + Sync>;

/// One download at a time, shared by Add phone and Advanced, so closing the dialog doesn't lose one under way
/// (widgets/adb.py's AdbDownload).
#[derive(Clone)]
pub struct AdbDownloader {
    running: Arc<std::sync::atomic::AtomicBool>,
    job: DownloadJob,
}

impl AdbDownloader {
    pub fn new(job: DownloadJob) -> AdbDownloader {
        AdbDownloader { running: Arc::default(), job }
    }

    /// Google's platform-tools into downloaded_dir().
    pub fn system() -> AdbDownloader {
        AdbDownloader::new(Arc::new(|say| download_adb(&Web, None, say)))
    }

    pub fn running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Starts job (the download by default) unless one runs already; false then. progress and done come from its
    /// thread, and done always comes, even when the job panics.
    pub fn start(&self, job: Option<DownloadJob>, progress: Box<dyn Fn(&str) + Send>,
                 done: Box<dyn FnOnce(Result<String, String>) + Send>) -> bool {
        if self.running.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        let (running, job) = (self.running.clone(), job.unwrap_or_else(|| self.job.clone()));
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&*progress))).unwrap_or_else(|_| {
                log::error!("adb download failed");
                Err(tr("Couldn't download adb. Try again."))
            });
            running.store(false, std::sync::atomic::Ordering::SeqCst);
            done(result);
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::{mpsc, Arc};
    use zip::write::SimpleFileOptions;

    const ZIP_NAME: &str = "platform-tools_r37.0.1-win.zip";

    // The shape of Google's repository2-3.xml (trimmed from the real one: namespaces, the unprefixed remotePackage)
    const INDEX: &str = r#"<?xml version='1.0' encoding='utf-8'?>
<sdk:sdk-repository xmlns:sdk="http://schemas.android.com/sdk/android/repo/repository2/03"
    xmlns:common="http://schemas.android.com/repository/android/common/02"
    xmlns:generic="http://schemas.android.com/repository/android/generic/02"
    xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <remotePackage path="build-tools;34.0.0">
    <revision><major>34</major><minor>0</minor><micro>0</micro></revision>
    <archives><archive><complete><size>1</size><checksum type="sha1">00</checksum>
      <url>build-tools-win.zip</url></complete><host-os>windows</host-os></archive></archives>
  </remotePackage>
  <remotePackage path="platform-tools">
    <type-details xsi:type="generic:genericDetailsType" />
    <revision><major>37</major><minor>0</minor><micro>1</micro></revision>
    <uses-license ref="android-sdk-license" />
    <archives>
      <archive><complete><size>9054187</size><checksum type="sha1">477254aa5f903c15cf51001717bdf347fb6b53e0</checksum>
        <url>platform-tools_r37.0.1-linux.zip</url></complete><host-os>linux</host-os></archive>
      <archive><complete><size>{size}</size><checksum type="sha1">{sha1}</checksum>
        <url>{name}</url></complete><host-os>windows</host-os></archive>
    </archives>
  </remotePackage>
</sdk:sdk-repository>"#;

    const ALL: [&str; 8] = ["adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll", "libwinpthread-1.dll", "NOTICE.txt",
                            "source.properties", "fastboot.exe", "sqlite3.exe"];

    fn zip_of(names: &[&str], prefix: &str, folder: bool) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        if folder {
            z.add_directory(prefix, SimpleFileOptions::default()).unwrap();
        }
        for name in names {
            z.start_file(format!("{prefix}{name}"), SimpleFileOptions::default()).unwrap();
            z.write_all(name.as_bytes()).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    fn sha1(data: &[u8]) -> String {
        hex(ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, data).as_ref())
    }

    fn index_for(size: u64, sha1: &str, name: &str) -> Vec<u8> {
        INDEX.replace("{size}", &size.to_string()).replace("{sha1}", sha1).replace("{name}", name).into_bytes()
    }

    /// dl.google.com: the index and one zip.
    struct Google {
        zip: Vec<u8>,
        index: Vec<u8>,
        fail: Option<&'static str>,
        urls: Mutex<Vec<String>>,
    }

    impl Google {
        fn new() -> Google {
            Google::serving(zip_of(&ALL, "platform-tools/", true), None, ZIP_NAME)
        }

        fn serving(zip: Vec<u8>, listed: Option<Vec<u8>>, name: &str) -> Google {
            let listed = listed.unwrap_or_else(|| zip.clone());
            let index = index_for(listed.len() as u64, &sha1(&listed), name);
            Google { zip, index, fail: None, urls: Mutex::default() }
        }

        fn urls(&self) -> Vec<String> {
            self.urls.lock().unwrap().clone()
        }
    }

    impl Fetch for Google {
        fn open(&self, url: &str, _timeout: Duration) -> Result<(String, Box<dyn Read + Send>), FetchError> {
            self.urls.lock().unwrap().push(url.to_string());
            if self.fail.is_some_and(|f| url.contains(f)) {
                return Err(FetchError::Offline("timed out".into()));
            }
            let body = if url == INDEX_URL {
                self.index.clone()
            } else if url == format!("{REPOSITORY}{ZIP_NAME}") {
                self.zip.clone()
            } else {
                panic!("unexpected {url}")
            };
            Ok((url.to_string(), Box::new(Cursor::new(body))))
        }
    }

    struct Failing(fn(&str) -> FetchError);

    impl Fetch for Failing {
        fn open(&self, url: &str, _timeout: Duration) -> Result<(String, Box<dyn Read + Send>), FetchError> {
            Err((self.0)(url))
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        found.sort();
        found
    }

    fn sorted(list: &[&str]) -> Vec<String> {
        let mut list: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        list.sort();
        list
    }

    fn tmp() -> PathBuf {
        telescope_core::testing::scratch_dir("adb-download")
    }

    fn quiet(_: &str) {}

    #[test]
    fn the_index_gives_the_windows_zip_its_size_and_checksum() {
        let google = Google::new();
        let archive = find_archive(&google.index, "windows").unwrap();
        assert_eq!(archive, Archive { url: format!("{REPOSITORY}{ZIP_NAME}"), size: google.zip.len() as u64,
                                      sha1: sha1(&google.zip), revision: "37.0.1".into() });
        assert!(find_archive(&google.index, "linux").unwrap().url.ends_with("platform-tools_r37.0.1-linux.zip"));
    }

    #[test]
    fn the_index_can_only_point_next_to_itself() {
        for name in ["../evil.zip", "https://elsewhere.example/pt.zip", "sub/pt.zip", "pt.exe"] {
            assert!(find_archive(&Google::serving(Vec::new(), None, name).index, "windows").is_err(), "{name}");
        }
    }

    #[test]
    fn an_index_without_platform_tools_is_an_error() {
        let err = find_archive(b"<sdk-repository><remotePackage path='emulator'/></sdk-repository>", "windows").unwrap_err();
        assert!(err.contains("no platform-tools"));
    }

    #[test]
    fn it_downloads_from_google_and_keeps_only_what_adb_needs() {
        let root = tmp();
        let (google, dest) = (Google::new(), root.join("Telescope").join("platform-tools"));
        let said = Mutex::new(Vec::new());
        assert_eq!(download_adb(&google, Some(&dest), &|m| said.lock().unwrap().push(m.to_string())), Ok("37.0.1".into()));
        assert_eq!(google.urls(), vec![INDEX_URL.to_string(), format!("{REPOSITORY}{ZIP_NAME}")]);
        assert_eq!(names(&dest), sorted(&NEEDED));
        assert_eq!(std::fs::read(dest.join("adb.exe")).unwrap(), b"adb.exe");
        assert_eq!(names(dest.parent().unwrap()), vec!["platform-tools"]);
        assert!(said.lock().unwrap()[1].contains("37.0.1"));
    }

    #[test]
    fn a_zip_that_doesn_t_match_the_index_isn_t_used() {
        let root = tmp();
        let dest = root.join("platform-tools");
        let real = zip_of(&ALL, "platform-tools/", true);
        let mut listed = real.clone();
        *listed.last_mut().unwrap() = b'!';
        let why = download_adb(&Google::serving(real, Some(listed), ZIP_NAME), Some(&dest), &quiet).unwrap_err();
        assert!(why.contains("checksum"));
        assert!(!dest.exists());
        assert!(names(&root).is_empty());
    }

    #[test]
    fn a_bigger_download_than_listed_stops_early() {
        let google = Google::serving(zip_of(&ALL, "platform-tools/", true), Some(b"tiny".to_vec()), ZIP_NAME);
        assert_eq!(download_adb(&google, Some(&tmp().join("platform-tools")), &quiet), Err("Couldn't download adb. Try again.".into()));
    }

    #[test]
    fn a_zip_without_adb_isn_t_used() {
        let dest = tmp().join("platform-tools");
        let why = download_adb(&Google::serving(zip_of(&["fastboot.exe"], "platform-tools/", true), None, ZIP_NAME), Some(&dest), &quiet).unwrap_err();
        assert!(why.contains("adb.exe") && !dest.exists());
    }

    #[test]
    fn no_network_says_so_and_keeps_what_was_there() {
        for fail in ["repository2-3.xml", "platform-tools_r37"] {
            let root = tmp();
            let dest = root.join("platform-tools");
            std::fs::create_dir(&dest).unwrap();
            std::fs::write(dest.join("NOTICE.txt"), b"old").unwrap();
            let google = Google { fail: Some(fail), ..Google::new() };
            assert_eq!(download_adb(&google, Some(&dest), &quiet), Err("Couldn't reach Google. Check the internet connection and try again.".into()));
            assert_eq!(std::fs::read(dest.join("NOTICE.txt")).unwrap(), b"old");
            assert_eq!(names(&root), vec!["platform-tools"]);
        }
    }

    #[test]
    fn an_unchecked_certificate_isnt_blamed_on_the_internet() {
        let untrusted = Failing(|_| FetchError::Certificate("unable to get local issuer certificate".into()));
        let why = download_adb(&untrusted, Some(&tmp().join("platform-tools")), &quiet).unwrap_err();
        assert!(why.starts_with("Couldn't check Google's certificate") && !why.contains("internet connection"));
    }

    #[test]
    fn downloads_go_through_the_system_certificate_check() {
        let source = include_str!("adb_download.rs");
        let web = &source[source.find("impl Fetch for Web").unwrap()..source.find("fn is_certificate").unwrap()];
        assert!(web.contains("RootCerts::PlatformVerifier") && web.contains(".https_only(true)"));
    }

    #[test]
    fn a_rustls_certificate_error_counts_as_a_certificate_failure() {
        let bad = rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer);
        assert!(matches!(fetch_error(ureq::Error::Rustls(bad.clone())), FetchError::Certificate(_)));
        assert!(matches!(fetch_error(ureq::Error::Io(io::Error::other(bad))), FetchError::Certificate(_)));
        assert!(matches!(fetch_error(ureq::Error::HostNotFound), FetchError::Offline(_)));
        assert!(matches!(fetch_error(ureq::Error::StatusCode(503)), FetchError::Http(503)));
    }

    #[test]
    fn a_half_emptied_folder_is_replaced_whole() {
        let dest = tmp().join("platform-tools");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("stale.dll"), b"old").unwrap();
        assert!(download_adb(&Google::new(), Some(&dest), &quiet).is_ok());
        assert_eq!(names(&dest), sorted(&NEEDED));
    }

    #[test]
    fn adb_that_s_already_there_isn_t_fetched_again() {
        // The other dialog got it first, and its adb server may be running from that folder
        let (google, dest) = (Google::new(), tmp().join("platform-tools"));
        assert_eq!(download_adb(&google, Some(&dest), &quiet), Ok("37.0.1".into()));
        assert_eq!(google.urls().len(), 2);
        std::fs::write(dest.join("source.properties"), "Pkg.UserSrc=false\nPkg.Revision=37.0.1\n").unwrap();
        assert_eq!(download_adb(&google, Some(&dest), &quiet), Ok("37.0.1".into()));
        assert_eq!(google.urls().len(), 2);
        std::fs::remove_file(dest.join("source.properties")).unwrap();
        assert_eq!(download_adb(&google, Some(&dest), &quiet), Ok(String::new()));
    }

    #[test]
    fn a_cut_short_download_s_work_folder_is_cleared_later() {
        let root = tmp();
        let (stale, fresh) = (root.join(".adb-old"), root.join(".adb-running"));
        for folder in [&stale, &fresh] {
            std::fs::create_dir(folder).unwrap();
            std::fs::write(folder.join("platform-tools.zip"), b"part").unwrap();
        }
        let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
        std::fs::File::open(&stale).unwrap().set_modified(two_hours_ago).unwrap();
        assert!(download_adb(&Google::new(), Some(&root.join("platform-tools")), &quiet).is_ok());
        assert_eq!(names(&root), vec![".adb-running", "platform-tools"]);
    }

    #[test]
    fn a_server_error_says_so() {
        let failing = Failing(|_| FetchError::Http(503));
        assert_eq!(download_adb(&failing, Some(&tmp().join("pt")), &quiet), Err("Google's server said 503. Try again later.".into()));
    }

    #[test]
    fn an_index_redirected_off_https_isn_t_trusted() {
        struct Redirected(Vec<u8>);
        impl Fetch for Redirected {
            fn open(&self, _url: &str, _timeout: Duration) -> Result<(String, Box<dyn Read + Send>), FetchError> {
                Ok(("http://dl.google.com/android/repository/repository2-3.xml".into(), Box::new(Cursor::new(self.0.clone()))))
            }
        }
        let root = tmp();
        assert!(download_adb(&Redirected(Google::new().index), Some(&root.join("pt")), &quiet).is_err());
        assert!(!root.join("pt").exists());
    }

    #[test]
    fn a_version_that_isn_t_numbers_isn_t_shown() {
        let index = String::from_utf8(Google::new().index).unwrap()
            .replace("<major>37</major>", "<major><![CDATA[<a href=x>hi</a>]]></major>");
        assert_eq!(find_archive(index.as_bytes(), "windows").unwrap().revision, "");
    }

    #[test]
    fn a_zip_shorter_than_listed_is_refused_even_with_its_own_checksum() {
        let root = tmp();
        let (real, dest) = (zip_of(&ALL, "platform-tools/", true), root.join("platform-tools"));
        let mut google = Google::serving(real.clone(), None, ZIP_NAME);
        google.index = index_for(real.len() as u64 + 10, &sha1(&real), ZIP_NAME);
        assert_eq!(download_adb(&google, Some(&dest), &quiet), Err("The download didn't match Google's checksum, so it wasn't used. Try again.".into()));
        assert!(!dest.exists());
        assert!(names(&root).is_empty());
    }

    #[test]
    fn an_index_listing_a_huge_zip_is_refused_before_downloading_it() {
        let mut google = Google::new();
        google.index = index_for(MAX_ZIP + 1, &"00".repeat(20), ZIP_NAME);
        let why = download_adb(&google, Some(&tmp().join("platform-tools")), &quiet).unwrap_err();
        assert!(why.contains("wasn't what Telescope expected"));
        assert_eq!(google.urls(), vec![INDEX_URL.to_string()]);
    }

    #[test]
    fn an_archive_with_another_checksum_type_is_skipped() {
        let index = String::from_utf8(Google::new().index).unwrap().replace(r#"type="sha1""#, r#"type="sha256""#);
        assert!(find_archive(index.as_bytes(), "windows").unwrap_err().contains("no platform-tools"));
    }

    #[test]
    fn a_failed_move_puts_the_old_folder_back() {
        let root = tmp();
        let dest = root.join("platform-tools");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("stale.dll"), b"old").unwrap();
        let moves = Mutex::new(0);
        let flaky = |from: &Path, to: &Path| -> io::Result<()> {
            let mut n = moves.lock().unwrap();
            *n += 1;
            // The unpacked folder going into dest
            if *n == 2 {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"));
            }
            std::fs::rename(from, to)
        };
        let why = download_with(&Google::new(), Some(&dest), &quiet, &flaky).unwrap_err();
        assert!(why.starts_with("Couldn't put adb in place"));
        assert_eq!(std::fs::read(dest.join("stale.dll")).unwrap(), b"old");
        assert_eq!(names(&root), vec!["platform-tools"]);
    }

    #[test]
    fn two_downloads_at_once_fetch_only_once() {
        struct Held {
            google: Google,
            at_zip: Mutex<mpsc::Sender<()>>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        impl Fetch for Held {
            fn open(&self, url: &str, timeout: Duration) -> Result<(String, Box<dyn Read + Send>), FetchError> {
                if url != INDEX_URL {
                    self.at_zip.lock().unwrap().send(()).unwrap();
                    self.release.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
                }
                self.google.open(url, timeout)
            }
        }
        let (at_tx, at_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let held = Arc::new(Held { google: Google::new(), at_zip: Mutex::new(at_tx), release: Mutex::new(release_rx) });
        let dest = tmp().join("platform-tools");
        let run = |held: Arc<Held>, dest: PathBuf| std::thread::spawn(move || download_adb(&*held, Some(&dest), &quiet));
        let first = run(held.clone(), dest.clone());
        at_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let second = run(held.clone(), dest.clone());
        // Long enough for it to be waiting on the lock
        std::thread::sleep(Duration::from_millis(200));
        release_tx.send(()).unwrap();
        assert!(first.join().unwrap().is_ok());
        assert!(second.join().unwrap().is_ok());
        assert_eq!(held.google.urls().len(), 2);
    }

    #[test]
    fn a_blocked_download_says_google_blocks_some_countries_and_offers_picking_one() {
        for code in [403u16, 404, 451] {
            let why = match code {
                403 => download_adb(&Failing(|_| FetchError::Http(403)), Some(&tmp().join("pt")), &quiet),
                404 => download_adb(&Failing(|_| FetchError::Http(404)), Some(&tmp().join("pt")), &quiet),
                _ => download_adb(&Failing(|_| FetchError::Http(451)), Some(&tmp().join("pt")), &quiet),
            }.unwrap_err();
            assert!(why.contains(&format!("said {code}")) && why.contains("some countries") && why.contains("adb.exe or zip"), "{why}");
        }
    }

    #[test]
    fn an_adb_picked_by_hand_is_kept_with_only_what_it_needs() {
        let root = tmp();
        let theirs = root.join("Downloads").join("platform-tools");
        std::fs::create_dir_all(&theirs).unwrap();
        for name in ["adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll", "fastboot.exe"] {
            std::fs::write(theirs.join(name), name).unwrap();
        }
        std::fs::write(theirs.join("source.properties"), "Pkg.Revision=36.0.0\n").unwrap();
        let dest = root.join("Telescope").join("platform-tools");
        assert_eq!(use_own_adb(&theirs.join("adb.exe"), Some(&dest)), Ok("36.0.0".into()));
        assert_eq!(names(&dest), vec!["AdbWinApi.dll", "AdbWinUsbApi.dll", "adb.exe", "source.properties"]);
        // Theirs is copied, not moved
        assert!(theirs.join("fastboot.exe").exists());
        assert_eq!(names(dest.parent().unwrap()), vec!["platform-tools"]);
    }

    #[test]
    fn a_platform_tools_zip_picked_by_hand_is_unpacked() {
        for prefix in ["platform-tools/", ""] {
            let root = tmp();
            let zip_path = root.join("pt.zip");
            std::fs::write(&zip_path, zip_of(&["adb.exe", "AdbWinApi.dll", "fastboot.exe"], prefix, false)).unwrap();
            let dest = root.join("Telescope").join("platform-tools");
            assert_eq!(use_own_adb(&zip_path, Some(&dest)), Ok(String::new()));
            assert_eq!(names(&dest), vec!["AdbWinApi.dll", "adb.exe"]);
        }
    }

    #[test]
    fn a_pick_without_adb_in_it_isn_t_kept() {
        let root = tmp();
        let zip_path = root.join("other.zip");
        std::fs::write(&zip_path, zip_of(&["readme.txt"], "", false)).unwrap();
        let dest = root.join("Telescope").join("platform-tools");
        assert_eq!(use_own_adb(&zip_path, Some(&dest)), Err("That doesn't have adb.exe in it.".into()));
        assert!(!dest.exists());
        std::fs::write(root.join("broken.zip"), b"not a zip").unwrap();
        assert_eq!(use_own_adb(&root.join("broken.zip"), Some(&dest)), Err("Couldn't copy adb from there.".into()));
    }

    #[test]
    fn asking_while_a_download_runs_doesn_t_ask_or_start_another() {
        let calls = Arc::new(Mutex::new(0));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let c = calls.clone();
        let downloader = AdbDownloader::new(Arc::new(move |_say| {
            *c.lock().unwrap() += 1;
            release_rx.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
            Ok("37.0.1".into())
        }));
        let (done_tx, done_rx) = mpsc::channel();
        assert!(downloader.start(None, Box::new(|_| {}), Box::new(move |r| done_tx.send(r).unwrap())));
        assert!(downloader.running());
        assert!(!downloader.start(None, Box::new(|_| {}), Box::new(|_| panic!("a second one ran"))));
        release_tx.send(()).unwrap();
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(5)).unwrap(), Ok("37.0.1".into()));
        assert_eq!(*calls.lock().unwrap(), 1);
        assert!(!downloader.running());
    }

    #[test]
    fn a_download_that_raises_still_reports_back() {
        let downloader = AdbDownloader::new(Arc::new(|_say| panic!("boom")));
        let (tx, rx) = mpsc::channel();
        downloader.start(None, Box::new(|_| {}), Box::new(move |r| tx.send(r).unwrap()));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Err("Couldn't download adb. Try again.".into()));
        assert!(!downloader.running());
    }
}
