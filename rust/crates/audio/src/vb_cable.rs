//! Fetching VB-Audio Virtual Cable and opening its setup (virtual_mic.py's Windows half). VB-Cable isn't part of
//! Telescope: its readme doesn't allow putting it inside another setup without VB-Audio's agreement, so their setup
//! runs as it is, with its own windows, and the person installing it sees whose it is first.

use ring::digest::{Context, SHA256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use telescope_core::i18n::tr;

pub const VB_CABLE_URL: &str = "https://vb-audio.com/Cable/";
/// The pack VB-Audio's page links, pinned: a newer one fails the check and the card goes back to the link above.
pub const VB_CABLE_PACK_URL: &str = "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip";
pub const VB_CABLE_PACK_SHA256: &str = "b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb";
/// Signed by Vincent Burel, so UAC names VB-Audio's author.
pub const VB_CABLE_SETUP: &str = "VBCABLE_Setup_x64.exe";
pub const VB_CABLE_SETUP_SHA256: &str = "734c35dfa6d98f48782a451633ceb471166ec70d60482fd89a1123d0ee3c4f41";
// What VB-Audio asks anyone passing VB-Cable on to say (the pack's readme.txt)
pub const VB_CABLE_ORIGIN: &str = "The origin of VB-CABLE : www.vb-cable.com.";
pub const VB_CABLE_DONATIONWARE: &str = "VB-CABLE is a donationware, all participations are welcome.";
/// ERROR_CANCELLED: no to the UAC prompt.
pub const ERROR_CANCELLED: i32 = 1223;
const PACK: &str = "VBCABLE_Driver_Pack.zip";
/// The pack is a few MB; anything far bigger isn't it.
const MAX_PACK: u64 = 64 << 20;

/// What the pack and its setup have to hash to, and how big the pack may be (tests pin their own).
#[derive(Clone, Debug)]
pub struct Pins {
    pub pack_sha256: String,
    pub setup_sha256: String,
    pub max_pack: u64,
}

impl Default for Pins {
    fn default() -> Self {
        Pins { pack_sha256: VB_CABLE_PACK_SHA256.into(), setup_sha256: VB_CABLE_SETUP_SHA256.into(), max_pack: MAX_PACK }
    }
}

/// Why a download didn't come.
#[derive(Debug)]
pub enum FetchError {
    /// Reached, but it said no.
    Http(u16),
    Certificate(String),
    Offline(String),
    Other(String),
}

pub type Fetch<'a> = &'a dyn Fn(&str, Duration) -> Result<Box<dyn Read>, FetchError>;

/// The real web, through the system's certificate store (telescope.web.urlopen).
pub fn web(url: &str, timeout: Duration) -> Result<Box<dyn Read>, FetchError> {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .https_only(true)
        // Not timeout_global: that would cap the whole download, so a slow but moving one would fail. A stall is what times out
        .timeout_resolve(Some(timeout)).timeout_connect(Some(timeout)).timeout_send_request(Some(timeout)).timeout_recv_response(Some(timeout)).timeout_per_read(Some(timeout))
        .user_agent(concat!("Telescope/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let response = agent.get(url).call().map_err(fetch_error)?;
    Ok(Box::new(response.into_body().into_reader()))
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

fn sha256(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut ctx = Context::new(&SHA256);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// A flat pack: anything with a folder in its name, or hidden, isn't what was checked, so it's left out.
fn flat_name(name: &str) -> bool {
    !name.contains(['/', '\\']) && !name.starts_with('.') && !name.is_empty() && name != ".."
}

/// An earlier try's folder, each a few MB. Only ones a day old: a newer one's setup may still be open.
fn clear_old_tries(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else { return };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("telescope-vbcable-") {
            continue;
        }
        let old = entry.metadata().and_then(|m| m.modified()).ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok()).is_some_and(|age| age > Duration::from_secs(86400));
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn new_folder(temp: &Path) -> std::io::Result<PathBuf> {
    for n in 0..1000u32 {
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let path = temp.join(format!("telescope-vbcable-{}{:08x}", std::process::id(), nanos.wrapping_add(n)));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free folder"))
}

/// Fetches VB-Audio's pack and unpacks it; the setup's path, or why not. folder None makes a fresh one in temp.
pub fn download_vb_cable(folder: Option<&Path>, temp: &Path, fetch: Fetch, pins: &Pins) -> Result<PathBuf, String> {
    let folder = match folder {
        Some(f) => f.to_path_buf(),
        None => {
            clear_old_tries(temp);
            new_folder(temp).map_err(|e| {
                log::warn!("VB-Cable download: {e}");
                tr("Couldn't download VB-Cable. Try again, or get it from their site.")
            })?
        }
    };
    let pack = folder.join(PACK);
    let fetched = (|| -> Result<(), FetchError> {
        let mut body = fetch(VB_CABLE_PACK_URL, Duration::from_secs(30))?;
        let mut out = std::fs::File::create(&pack).map_err(|e| FetchError::Other(e.to_string()))?;
        let mut got = 0u64;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = body.read(&mut buf).map_err(|e| FetchError::Other(e.to_string()))?;
            if n == 0 {
                return Ok(());
            }
            got += n as u64;
            if got > pins.max_pack {
                return Err(FetchError::Other("it's much bigger than VB-Audio's pack".into()));
            }
            std::io::Write::write_all(&mut out, &buf[..n]).map_err(|e| FetchError::Other(e.to_string()))?;
        }
    })();
    if let Err(e) = fetched {
        log::warn!("VB-Cable download: {e:?}");
        return Err(match e {
            FetchError::Http(_) => tr("Couldn't download VB-Cable from VB-Audio's site. Try again later, or get it from their site."),
            FetchError::Certificate(_) => tr("Couldn't check vb-audio.com's certificate, so nothing was downloaded. Security software or a network that inspects HTTPS can cause this."),
            FetchError::Offline(_) => tr("Couldn't reach vb-audio.com. Check the internet connection and try again."),
            FetchError::Other(_) => tr("Couldn't download VB-Cable. Try again, or get it from their site."),
        });
    }
    if sha256(&pack).ok().as_deref() != Some(pins.pack_sha256.as_str()) {
        return Err(tr("VB-Audio's download has changed since this version of Telescope. Get it from their site instead."));
    }
    let unpacked = (|| -> Result<(), String> {
        let mut z = zip::ZipArchive::new(std::fs::File::open(&pack).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        for i in 0..z.len() {
            let mut entry = z.by_index(i).map_err(|e| e.to_string())?;
            let name = entry.name().map_err(|e| e.to_string())?.to_string();
            if !flat_name(&name) || entry.is_dir() {
                continue;
            }
            let mut out = std::fs::File::create(folder.join(&name)).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if let Err(e) = unpacked {
        log::warn!("VB-Cable unpack: {e}");
        return Err(tr("Couldn't unpack VB-Cable. Try again, or get it from their site."));
    }
    let setup = folder.join(VB_CABLE_SETUP);
    if !setup.exists() {
        return Err(tr("VB-Audio's download no longer has the setup file. Get it from their site instead."));
    }
    Ok(setup)
}

/// The setup runs as admin and reads the files beside it (and Windows looks there for DLLs), so the whole folder has
/// to be the pinned pack's files and nothing else, not just the setup.
fn folder_is_the_pack(folder: &Path, pins: &Pins) -> bool {
    let check = || -> Result<bool, String> {
        let pack = folder.join(PACK);
        if sha256(&pack).map_err(|e| e.to_string())? != pins.pack_sha256 {
            return Ok(false);
        }
        let mut z = zip::ZipArchive::new(std::fs::File::open(&pack).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let mut wanted = std::collections::BTreeMap::new();
        for i in 0..z.len() {
            let mut entry = z.by_index(i).map_err(|e| e.to_string())?;
            let name = entry.name().map_err(|e| e.to_string())?.to_string();
            if flat_name(&name) && !entry.is_dir() {
                let mut data = Vec::new();
                entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
                wanted.insert(name, data);
            }
        }
        let mut present: Vec<String> = std::fs::read_dir(folder).map_err(|e| e.to_string())?
            .flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        present.sort();
        let mut expected: Vec<String> = wanted.keys().cloned().chain([PACK.to_string()]).collect();
        expected.sort();
        expected.dedup();
        if present != expected {
            return Ok(false);
        }
        Ok(wanted.iter().all(|(name, data)| std::fs::read(folder.join(name)).is_ok_and(|d| &d == data)))
    };
    check().unwrap_or(false)
}

/// Opens a program with a ShellExecute verb in a folder; the Windows error code when it can't.
pub type Start<'a> = &'a dyn Fn(&Path, &str, &Path) -> Result<(), i32>;

/// Opens VB-Audio's setup as admin, after checking it and everything beside it once more; "" once it's open, else why
/// not.
pub fn run_vb_cable_setup(setup: &Path, start: Start, pins: &Pins) -> String {
    let folder = setup.parent().unwrap_or(Path::new("."));
    if sha256(setup).ok().as_deref() != Some(pins.setup_sha256.as_str()) || !folder_is_the_pack(folder, pins) {
        return tr("VB-Cable's setup file changed after the check, so Telescope didn't open it.");
    }
    // the setup's own folder, wherever it looks for its files
    match start(setup, "runas", folder) {
        Ok(()) => String::new(),
        Err(ERROR_CANCELLED) => tr("Windows didn't get permission to install VB-Cable."),
        Err(code) => {
            log::warn!("VB-Cable setup: Windows error {code}");
            tr("Couldn't open VB-Cable's setup. Try again, or get it from their site.")
        }
    }
}

/// ShellExecuteW: with "runas", UAC asks for the setup itself.
#[cfg(windows)]
pub fn start_as_admin(program: &Path, verb: &str, folder: &Path) -> Result<(), i32> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(hwnd: *mut std::ffi::c_void, op: *const u16, file: *const u16, params: *const u16,
                         dir: *const u16, show: i32) -> isize;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLastError() -> u32;
    }
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain([0]).collect::<Vec<u16>>();
    let (op, file, dir) = (wide(verb.as_ref()), wide(program.as_os_str()), wide(folder.as_os_str()));
    // SAFETY: every string is NUL-terminated UTF-16 that outlives the call; SW_SHOWNORMAL is 1
    let result = unsafe { ShellExecuteW(std::ptr::null_mut(), op.as_ptr(), file.as_ptr(), std::ptr::null(), dir.as_ptr(), 1) };
    if result > 32 {
        return Ok(());
    }
    // SAFETY: no arguments
    Err(unsafe { GetLastError() } as i32)
}

#[cfg(not(windows))]
pub fn start_as_admin(_program: &Path, _verb: &str, _folder: &Path) -> Result<(), i32> {
    Err(50) // ERROR_NOT_SUPPORTED: only Windows has VB-Cable's setup
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Write;

    pub fn pack(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    const SETUP: &[u8] = b"VB-Audio's setup";

    fn good() -> Vec<u8> {
        pack(&[(VB_CABLE_SETUP, SETUP), ("vbMmeCable64_win10.inf", b"inf"), ("readme.txt", b"hi")])
    }

    fn hex(data: &[u8]) -> String {
        ring::digest::digest(&SHA256, data).as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The fake pack pinned the way the real one is.
    fn pinned(pack: &[u8]) -> Pins {
        Pins { pack_sha256: hex(pack), setup_sha256: hex(SETUP), max_pack: MAX_PACK }
    }

    fn serving(data: Vec<u8>, seen: &RefCell<Vec<String>>) -> impl Fn(&str, Duration) -> Result<Box<dyn Read>, FetchError> + '_ {
        move |url, _| {
            seen.borrow_mut().push(url.to_string());
            Ok(Box::new(std::io::Cursor::new(data.clone())))
        }
    }

    pub fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("telescope-vbc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn unpacked(dir: &Path) -> PathBuf {
        let seen = RefCell::new(Vec::new());
        let setup = download_vb_cable(Some(dir), dir, &serving(good(), &seen), &pinned(&good()));
        setup.unwrap()
    }

    #[test]
    fn the_pack_comes_from_vb_audio_and_unpacks_next_to_its_setup() {
        let dir = scratch("pack");
        let seen = RefCell::new(Vec::new());
        let setup = download_vb_cable(Some(&dir), &dir, &serving(good(), &seen), &pinned(&good())).unwrap();
        assert_eq!(*seen.borrow(), vec!["https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip".to_string()]);
        assert!(setup == dir.join("VBCABLE_Setup_x64.exe") && std::fs::read(&setup).unwrap() == SETUP);
        assert!(dir.join("vbMmeCable64_win10.inf").exists()); // the setup needs the driver files beside it
    }

    #[test]
    fn a_changed_pack_isnt_unpacked() {
        let dir = scratch("changed");
        let seen = RefCell::new(Vec::new());
        let mut changed = good();
        changed.push(b'x');
        let err = download_vb_cable(Some(&dir), &dir, &serving(changed, &seen), &pinned(&good())).unwrap_err();
        assert!(err.contains("changed"));
        assert!(!dir.join("VBCABLE_Setup_x64.exe").exists());
    }

    #[test]
    fn paths_in_the_pack_stay_in_its_folder() {
        let dir = scratch("paths");
        let evil = pack(&[(VB_CABLE_SETUP, SETUP), ("../escape.exe", b"x"), ("sub/dir.dll", b"x")]);
        let folder = dir.join("pack");
        std::fs::create_dir(&folder).unwrap();
        let seen = RefCell::new(Vec::new());
        download_vb_cable(Some(&folder), &dir, &serving(evil.clone(), &seen), &pinned(&evil)).unwrap();
        assert!(!dir.join("escape.exe").exists() && !folder.join("sub").exists());
    }

    fn failing(e: fn() -> FetchError) -> impl Fn(&str, Duration) -> Result<Box<dyn Read>, FetchError> {
        move |_, _| Err(e())
    }

    #[test]
    fn a_failed_download_says_so_plainly() {
        let dir = scratch("offline");
        let err = download_vb_cable(Some(&dir), &dir, &failing(|| FetchError::Offline("no route to host".into())), &Pins::default()).unwrap_err();
        assert_eq!(err, "Couldn't reach vb-audio.com. Check the internet connection and try again.");
    }

    #[test]
    fn an_unchecked_certificate_says_so() {
        let dir = scratch("cert");
        let err = download_vb_cable(Some(&dir), &dir, &failing(|| FetchError::Certificate("unable to get local issuer certificate".into())), &Pins::default()).unwrap_err();
        assert!(err.starts_with("Couldn't check vb-audio.com's certificate"));
        let bad = rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer);
        assert!(matches!(fetch_error(ureq::Error::Rustls(bad)), FetchError::Certificate(_)));
    }

    #[test]
    fn a_refused_download_doesnt_blame_the_internet() {
        let dir = scratch("refused");
        let err = download_vb_cable(Some(&dir), &dir, &failing(|| FetchError::Http(404)), &Pins::default()).unwrap_err();
        assert!(err.contains("Couldn't download VB-Cable"));
        assert!(matches!(fetch_error(ureq::Error::StatusCode(404)), FetchError::Http(404)));
    }

    #[test]
    fn a_download_far_bigger_than_the_pack_stops() {
        let dir = scratch("big");
        let seen = RefCell::new(Vec::new());
        let pins = Pins { max_pack: good().len() as u64 - 1, ..pinned(&good()) };
        let err = download_vb_cable(Some(&dir), &dir, &serving(good(), &seen), &pins).unwrap_err();
        assert!(err.contains("Couldn't download VB-Cable"));
    }

    #[test]
    fn the_setup_opens_as_admin_only_when_its_hash_still_matches() {
        let dir = scratch("admin");
        let setup = unpacked(&dir);
        let calls = RefCell::new(Vec::new());
        let start = |p: &Path, verb: &str, cwd: &Path| {
            calls.borrow_mut().push((p.to_path_buf(), verb.to_string(), cwd.to_path_buf()));
            Ok(())
        };
        assert_eq!(run_vb_cable_setup(&setup, &start, &pinned(&good())), "");
        assert_eq!(*calls.borrow(), vec![(setup.clone(), "runas".to_string(), dir.clone())]);
        std::fs::write(&setup, b"swapped after the check").unwrap();
        assert!(run_vb_cable_setup(&setup, &start, &pinned(&good())).contains("changed"));
        assert_eq!(calls.borrow().len(), 1);
    }

    #[test]
    fn nothing_runs_as_admin_from_a_folder_someone_changed() {
        let tampers: [fn(&Path); 3] = [
            |f| std::fs::write(f.join("version.dll"), b"planted").unwrap(), // Windows looks beside the exe for DLLs
            |f| std::fs::write(f.join("vbMmeCable64_win10.inf"), b"swapped").unwrap(),
            |f| std::fs::remove_file(f.join("readme.txt")).unwrap(),
        ];
        for (i, tamper) in tampers.iter().enumerate() {
            let dir = scratch(&format!("tamper{i}"));
            let setup = unpacked(&dir);
            tamper(&dir);
            let calls = RefCell::new(0);
            let start = |_: &Path, _: &str, _: &Path| {
                *calls.borrow_mut() += 1;
                Ok(())
            };
            assert!(run_vb_cable_setup(&setup, &start, &pinned(&good())).contains("changed"));
            assert_eq!(*calls.borrow(), 0);
        }
    }

    #[test]
    fn saying_no_to_uac_is_reported_plainly() {
        let dir = scratch("uac");
        let setup = unpacked(&dir);
        assert_eq!(run_vb_cable_setup(&setup, &|_, _, _| Err(ERROR_CANCELLED), &pinned(&good())),
                   "Windows didn't get permission to install VB-Cable.");
    }

    #[cfg(unix)] // a folder can't be opened as a file to age it
    #[test]
    fn an_earlier_tries_folder_is_cleared_first() {
        let temp = scratch("earlier");
        let old = temp.join("telescope-vbcable-old");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("VBCABLE_Setup_x64.exe"), b"x").unwrap();
        let two_days = SystemTime::now() - Duration::from_secs(2 * 86400);
        std::fs::File::open(&old).unwrap().set_modified(two_days).unwrap();
        let recent = temp.join("telescope-vbcable-recent"); // its setup may still be open
        std::fs::create_dir(&recent).unwrap();
        let keep = temp.join("something-else");
        std::fs::create_dir(&keep).unwrap();
        let seen = RefCell::new(Vec::new());
        let setup = download_vb_cable(None, &temp, &serving(good(), &seen), &pinned(&good())).unwrap();
        assert_eq!(setup.parent().unwrap().parent().unwrap(), temp);
        assert!(!old.exists() && keep.exists() && recent.exists());
    }

    #[test]
    fn a_hidden_file_in_the_pack_isn_t_unpacked_and_one_planted_later_blocks_the_setup() {
        let dir = scratch("hidden");
        let p = pack(&[(VB_CABLE_SETUP, SETUP), ("readme.txt", b"hi"), (".hidden", b"x")]);
        let seen = RefCell::new(Vec::new());
        let setup = download_vb_cable(Some(&dir), &dir, &serving(p.clone(), &seen), &pinned(&p)).unwrap();
        assert!(!dir.join(".hidden").exists());
        let calls = RefCell::new(0);
        let start = |_: &Path, _: &str, _: &Path| {
            *calls.borrow_mut() += 1;
            Ok(())
        };
        assert_eq!(run_vb_cable_setup(&setup, &start, &pinned(&p)), "");
        std::fs::write(dir.join(".hidden"), b"planted").unwrap();
        assert!(run_vb_cable_setup(&setup, &start, &pinned(&p)).contains("changed"));
        assert_eq!(*calls.borrow(), 1);
    }
}
