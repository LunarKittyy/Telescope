//! test_vision_pack.py, by name.

use super::*;
use std::io::Cursor;
use telescope_core::testing::scratch_dir;

const FILES: &[(&str, &[u8])] = &[("site/mediapipe/__init__.py", b""), ("models/a.task", b"model"), ("NOTICES.txt", b"licences")];

pub(crate) fn zipped(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in files {
        z.start_file(*name, opts).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn tree_of(files: &[(&str, &[u8])], dir: &Path) -> String {
    let folder = dir.join("expected");
    for (name, data) in files {
        let path = folder.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
    tree_hash(&folder).unwrap()
}

/// A pack in vision_pack.json for this machine and the bytes the opener serves.
struct Fixture {
    folder: PathBuf,
    tree: String,
    info: PackInfo,
    served: std::sync::Mutex<Vec<u8>>,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = scratch_dir("vision-pack");
        let tree = tree_of(FILES, &tmp);
        let data = zipped(FILES);
        let info = PackInfo::from_json(&serde_json::json!({"linux": {"id": tree, "size": data.len(), "files": 3}}), Some("linux"));
        Fixture { folder: tmp.join("vision"), tree, info, served: std::sync::Mutex::new(data) }
    }

    fn download(&self) -> Result<PathBuf, PackError> {
        let served = self.served.lock().unwrap().clone();
        let opener = move |_url: &str, _t: Duration| -> io::Result<Box<dyn Read>> { Ok(Box::new(Cursor::new(served.clone()))) };
        download(&self.info, &self.folder, &mut |_, _| {}, &|| false, &opener, Some("http://x"))
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut n: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    n.sort();
    n
}

#[test]
fn test_pack_hash_uses_case_sensitive_path_order_on_every_platform() {
    let tmp = scratch_dir("vision-hash");
    let files: &[(&str, &[u8])] = &[("site/pkg/METADATA", b"upper"), ("site/pkg/entry.py", b"lower"), ("site/pkg-info/RECORD", b"sibling")];
    let mut sorted: Vec<&(&str, &[u8])> = files.iter().collect();
    sorted.sort_by_key(|(n, _)| n.split('/').collect::<Vec<_>>());
    let manifest: String = sorted.iter().map(|(n, d)| format!("{n}\0{}\n", sha256_hex(d))).collect();
    assert_eq!(tree_of(files, &tmp), sha256_hex(manifest.as_bytes()));
}

#[test]
fn test_a_good_download_is_unpacked_under_its_id_and_older_packs_go() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.folder.join("0123456789abcdef")).unwrap();
    std::fs::write(f.folder.join(".old.zip.part"), b"x").unwrap();
    let served = f.served.lock().unwrap().clone();
    let size = served.len() as u64;
    let opener = move |_url: &str, _t: Duration| -> io::Result<Box<dyn Read>> { Ok(Box::new(Cursor::new(served.clone()))) };
    let mut seen = Vec::new();
    let last = download(&f.info, &f.folder, &mut |done, _| seen.push(done), &|| false, &opener, None).unwrap();
    assert_eq!(last, f.folder.join(&f.tree[..16]));
    assert_eq!(std::fs::read(last.join("models").join("a.task")).unwrap(), b"model");
    assert!(f.info.installed(&f.folder));
    assert_eq!(names(&f.folder), [f.tree[..16].to_string()]);
    assert_eq!(seen.last(), Some(&size));
}

#[test]
fn test_a_download_with_other_files_is_refused_and_leaves_nothing() {
    let f = Fixture::new();
    *f.served.lock().unwrap() = zipped(&[FILES[0], ("models/a.task", b"something else"), FILES[2]]);
    let e = f.download().unwrap_err();
    assert!(e.0.contains("didn't match"), "{e}");
    assert!(!f.info.installed(&f.folder));
    assert!(names(&f.folder).is_empty());
}

#[test]
fn test_a_download_that_escapes_its_folder_is_refused() {
    let f = Fixture::new();
    *f.served.lock().unwrap() = zipped(&[("../evil.py", b"x")]);
    assert!(f.download().unwrap_err().0.contains("unexpected paths"));
    assert!(!f.folder.parent().unwrap().join("evil.py").exists());
}

#[test]
fn test_a_download_much_bigger_than_expected_is_stopped() {
    let f = Fixture::new();
    let mut big = zipped(FILES);
    big.extend(vec![0u8; 10_000]);
    *f.served.lock().unwrap() = big;
    assert!(f.download().unwrap_err().0.contains("bigger"));
}

#[test]
fn test_a_dropped_connection_says_to_check_the_internet() {
    let f = Fixture::new();
    let offline = |_url: &str, _t: Duration| -> io::Result<Box<dyn Read>> { Err(io::Error::other("no route")) };
    let e = download(&f.info, &f.folder, &mut |_, _| {}, &|| false, &offline, None).unwrap_err();
    assert!(e.0.contains("internet connection"));
}

#[test]
fn test_a_computer_without_a_pack_gets_no_download() {
    let f = Fixture::new();
    let none = PackInfo::from_json(&serde_json::json!({"linux": {"id": f.tree, "size": 1}}), None);
    assert_eq!(none.asset_name(), None);
    let opener = |_url: &str, _t: Duration| -> io::Result<Box<dyn Read>> { unreachable!() };
    let e = download(&none, &f.folder, &mut |_, _| {}, &|| false, &opener, None).unwrap_err();
    assert!(e.0.contains("no download for this computer"));
}

#[test]
fn test_the_asset_is_named_by_platform_and_pack_id() {
    let f = Fixture::new();
    assert_eq!(f.info.asset_name(), Some(format!("Telescope-vision-linux-{}.zip", &f.tree[..16])));
}

#[test]
fn test_the_committed_packs_are_for_both_platforms() {
    let info: Value = serde_json::from_str(PACK_INFO).unwrap();
    let mut keys: Vec<&String> = info.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["linux", "windows"]);
    for entry in info.as_object().unwrap().values() {
        assert_eq!(entry["id"].as_str().unwrap().len(), 64);
        assert!(entry["size"].as_u64().unwrap() > 1_000_000);
    }
}

#[test]
fn a_cancelled_download_stops_and_leaves_nothing() {
    let f = Fixture::new();
    let served = f.served.lock().unwrap().clone();
    let opener = move |_url: &str, _t: Duration| -> io::Result<Box<dyn Read>> { Ok(Box::new(Cursor::new(served.clone()))) };
    let e = download(&f.info, &f.folder, &mut |_, _| {}, &|| true, &opener, None).unwrap_err();
    assert_eq!(e.0, "Cancelled.");
    assert!(names(&f.folder).is_empty());
}

#[test]
fn the_pack_folder_is_the_python_apps() {
    let dir = default_dir();
    assert!(dir.ends_with("telescope/vision") || dir.ends_with("Telescope\\vision") || dir.ends_with("vision"));
    let info = PackInfo::current();
    if platform_key() == Some("linux") {
        assert_eq!(info.pack_id().unwrap().len(), 16);
        assert_eq!(info.folder(Path::new("/v")), PathBuf::from("/v").join(info.pack_id().unwrap()));
    }
}

#[test]
fn a_dev_profile_keeps_its_packs_in_its_own_folder() {
    let env = |vars: &'static [(&'static str, &'static str)]| move |name: &str| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| std::ffi::OsString::from(v));
    let dev = env(&[("TELESCOPE_DEV_PROFILE", "/tmp/dev-profile"), ("XDG_DATA_HOME", "/real/share"), ("HOME", "/real")]);
    assert_eq!(dir_in("vision", &dev), PathBuf::from("/tmp/dev-profile/vision"));
    assert_eq!(dir_in("matting", &dev), PathBuf::from("/tmp/dev-profile/matting"));
    if cfg!(not(windows)) {
        let real = env(&[("XDG_DATA_HOME", "/real/share"), ("HOME", "/real")]);
        assert_eq!(dir_in("vision", &real), PathBuf::from("/real/share/telescope/vision"));
    }
}

/// A deflated zip of one file of zeros, which is a few KB for megabytes.
fn zeros_zipped(megabytes: usize) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    z.start_file("models/bomb.bin", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
    for _ in 0..megabytes {
        z.write_all(&vec![0u8; 1 << 20]).unwrap();
    }
    z.finish().unwrap().into_inner()
}

/// The same zip with the size it says the file has (in the local header and the directory) changed to `claim`.
fn claiming(mut data: Vec<u8>, claim: u32) -> Vec<u8> {
    for (signature, at) in [([0x50, 0x4b, 0x03, 0x04], 22), ([0x50, 0x4b, 0x01, 0x02], 24)] {
        let start = data.windows(4).position(|w| w == signature).unwrap();
        data[start + at..start + at + 4].copy_from_slice(&claim.to_le_bytes());
    }
    data
}

fn download_capped_with(f: &mut Fixture, served: Vec<u8>, cap: u64) -> Result<PathBuf, PackError> {
    f.info.size = served.len() as u64;
    let opener = move |_url: &str, _t: Duration| -> io::Result<Box<dyn Read>> { Ok(Box::new(Cursor::new(served.clone()))) };
    download_capped(&f.info, &f.folder, &mut |_, _| {}, &|| false, &opener, Some("http://x"), cap, TIMEOUT)
}

#[test]
fn a_small_zip_that_says_it_is_big_is_refused_before_anything_is_written() {
    let mut f = Fixture::new();
    let bomb = zeros_zipped(8);
    assert!(bomb.len() < 100_000, "the zip is small: {}", bomb.len());
    let e = download_capped_with(&mut f, bomb, 1 << 20).unwrap_err();
    assert!(e.0.contains("unpacks to more"), "{e}");
    assert!(names(&f.folder).is_empty());
}

#[test]
fn a_zip_that_lies_about_its_size_is_stopped_while_unpacking_and_leaves_nothing() {
    // The zip crate reads no more of an entry than the zip says it has, so what's written can't pass what the first check added up
    let mut f = Fixture::new();
    let bomb = claiming(zeros_zipped(8), 10);
    assert!(download_capped_with(&mut f, bomb, 1 << 20).is_err());
    assert!(names(&f.folder).is_empty(), "the unpacking folder is gone: {:?}", names(&f.folder));
}

#[test]
fn unpack_cleans_up_after_itself_and_takes_what_fits_under_the_ceiling() {
    let tmp = scratch_dir("vision-bomb");
    let (archive, staging) = (tmp.join("bomb.zip"), tmp.join("staging"));
    std::fs::write(&archive, claiming(zeros_zipped(8), 10)).unwrap();
    assert!(unpack(&archive, &staging, 1 << 20).is_err());
    assert!(!staging.exists());
    std::fs::write(&archive, zeros_zipped(8)).unwrap();
    assert!(matches!(unpack(&archive, &staging, 1 << 20), Err(Failed::Pack(_))));
    assert!(!staging.exists());
    // Under the ceiling the same files unpack as before
    std::fs::write(&archive, zipped(FILES)).unwrap();
    assert!(unpack(&archive, &staging, 1 << 20).is_ok());
    assert_eq!(std::fs::read(staging.join("models").join("a.task")).unwrap(), b"model");
    // A ceiling of exactly what's in it is enough and one byte less isn't
    let exact: u64 = FILES.iter().map(|(_, d)| d.len() as u64).sum();
    assert!(unpack(&archive, &staging, exact).is_ok());
    assert!(matches!(unpack(&archive, &staging, exact - 1), Err(Failed::Pack(_))));
}

#[test]
fn the_ceiling_leaves_room_for_the_real_packs() {
    // The Linux pack unpacks to 135,048,734 bytes
    assert!(MAX_UNPACKED > 135_048_734 && MAX_UNPACKED < 256 * 1024 * 1024);
}

#[test]
fn the_cutouts_download_is_named_and_kept_apart_from_the_vision_pack() {
    let info: Value = serde_json::from_str(MATTING_INFO).unwrap();
    for key in ["linux", "windows"] {
        let entry = PackInfo::from_json_for(Kind::Matting, &info, Some(key));
        let id = entry.id.clone().expect(key);
        assert!(id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit()), "{key}: {id}");
        assert_eq!(entry.asset_name().unwrap(), format!("Telescope-matting-{key}-{}.zip", &id[..16]));
        // about the size the notes promise and under the cap the app stops unpacking at
        assert!(entry.size > 1_000_000 && entry.size < MAX_UNPACKED, "{key}: {}", entry.size);
    }
    let vision = PackInfo::from_json(&serde_json::from_str(PACK_INFO).unwrap(), Some("linux"));
    assert_ne!(vision.asset_name().unwrap().split('-').nth(1), Some("matting"));
    assert_ne!(default_dir(), matting_dir());
    assert_eq!(default_dir().parent(), matting_dir().parent());
    // a build for another computer has none, and says so
    assert!(PackInfo::from_json_for(Kind::Matting, &info, None).asset_name().is_none());
}

/// Serves its bytes a few at a time, waiting before each piece; fails with `fail_after` pieces if set (a connection that went quiet).
struct Trickle {
    data: Cursor<Vec<u8>>,
    pause: Duration,
    pieces: usize,
    fail_after: Option<usize>,
}

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        std::thread::sleep(self.pause);
        if self.fail_after == Some(self.pieces) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no bytes"));
        }
        self.pieces += 1;
        let n = buf.len().min(48);
        self.data.read(&mut buf[..n])
    }
}

fn trickle_download(f: &Fixture, fail_after: Option<usize>, timeout: Duration) -> (Result<PathBuf, PackError>, Duration) {
    let served = f.served.lock().unwrap().clone();
    let opener = move |_url: &str, _t: Duration| -> io::Result<Box<dyn Read>> {
        Ok(Box::new(Trickle { data: Cursor::new(served.clone()), pause: Duration::from_millis(15), pieces: 0, fail_after }))
    };
    let start = std::time::Instant::now();
    let result = download_capped(&f.info, &f.folder, &mut |_, _| {}, &|| false, &opener, Some("http://x"), MAX_UNPACKED, timeout);
    (result, start.elapsed())
}

#[test]
fn a_slow_download_that_keeps_moving_finishes_even_past_the_timeout() {
    let f = Fixture::new();
    let timeout = Duration::from_millis(40);
    let (result, took) = trickle_download(&f, None, timeout);
    assert!(took > timeout * 3, "the download should outlast the timeout, took {took:?}");
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_download_that_goes_quiet_fails_as_a_connection_problem_and_leaves_nothing() {
    let f = Fixture::new();
    let (result, _) = trickle_download(&f, Some(2), Duration::from_millis(40));
    assert!(result.unwrap_err().0.contains("internet connection"));
    assert!(!f.folder.join(f.info.pack_id().unwrap()).exists());
}

#[test]
fn a_pack_zip_downloaded_by_hand_is_installed_like_the_download() {
    let f = Fixture::new();
    let zip = f.folder.parent().unwrap().join("Telescope-vision-linux-0123456789abcdef.zip");
    std::fs::write(&zip, f.served.lock().unwrap().clone()).unwrap();
    assert!(pack_zip_kind("Telescope-vision-windows-ab12 (1).zip") == Some(Kind::Vision) && pack_zip_kind("Telescope-matting-linux-ab.zip") == Some(Kind::Matting) && pack_zip_kind("my-plugin.zip").is_none());
    let last = install_zip(&f.info, &f.folder, &zip).unwrap();
    assert_eq!(last, f.info.folder(&f.folder));
    assert!(f.info.installed(&f.folder));
}

#[test]
fn a_pack_zip_for_another_version_is_refused_in_plain_words() {
    let f = Fixture::new();
    let other = zipped(&[("site/x.py", b"other")]);
    let zip = f.folder.parent().unwrap().join("Telescope-vision-linux-fedcba9876543210.zip");
    std::fs::write(&zip, other).unwrap();
    let e = install_zip(&f.info, &f.folder, &zip).unwrap_err();
    assert!(e.0.contains("didn't match"), "{e}");
    assert!(!f.info.installed(&f.folder));
}

#[test]
fn a_corrupted_pack_zip_is_refused_in_plain_words_and_leaves_nothing() {
    let f = Fixture::new();
    let mut bytes = f.served.lock().unwrap().clone();
    let mid = bytes.len() / 2;
    for b in &mut bytes[mid..mid + 16] {
        *b ^= 0xff;
    }
    let zip = f.folder.parent().unwrap().join("Telescope-vision-linux-0123456789abcdef.zip");
    std::fs::write(&zip, bytes).unwrap();
    let e = install_zip(&f.info, &f.folder, &zip).unwrap_err();
    eprintln!("corrupt zip error: {e}");
    assert!(!e.0.is_empty() && !f.info.installed(&f.folder));
    assert!(!f.folder.join(f.info.pack_id().unwrap()).exists());
    assert!(zip.exists());
}

#[test]
fn installing_a_zip_from_inside_the_pack_folder_leaves_the_zip_alone() {
    let f = Fixture::new();
    std::fs::create_dir_all(&f.folder).unwrap();
    std::fs::create_dir_all(f.folder.join("0123456789abcdef")).unwrap();
    std::fs::write(f.folder.join(".old.zip.part"), b"x").unwrap();
    let zip = f.folder.join("Telescope-vision-linux-0123456789abcdef.zip");
    std::fs::write(&zip, f.served.lock().unwrap().clone()).unwrap();
    install_zip(&f.info, &f.folder, &zip).unwrap();
    assert!(zip.exists(), "the chosen zip must stay");
    assert!(f.info.installed(&f.folder));
    assert_eq!(names(&f.folder), [f.tree[..16].to_string(), "Telescope-vision-linux-0123456789abcdef.zip".to_string()]);
}
