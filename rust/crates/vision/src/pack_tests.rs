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
