//! The window's plugin update check: telling about new releases and, with auto-update on, installing them (test_plugin_updater.py).

use serde_json::json;
use std::io::Write;
use std::sync::{Arc, Mutex};
use telescope_core::config::ConfigFile;
use telescope_core::testing::scratch_dir;
use telescope_plugin_host::package::{sha256_hex, PluginFolder};
use telescope_plugin_host::switches::{Switches, AUTO_UPDATE, CHECK_UPDATES};
use telescope_plugin_host::updater::PluginUpdater;
use telescope_plugin_host::updates::{Fetch, Reply};
use telescope_plugin_host::Installed;

const REPO_URL: &str = "https://github.com/someone/extra";

fn plugins() -> PluginFolder {
    let root = scratch_dir("updater");
    let folder = root.join("plugins");
    std::fs::create_dir(&folder).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in [&root, &folder] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    PluginFolder::new(folder, Switches::new(ConfigFile::at(root.join("telescope_config.json"))), [])
}

fn manifest(name: &str, version: &str) -> String {
    json!({"name": name, "title": name, "version": version, "api": 2, "repo": REPO_URL}).to_string()
}

fn install(p: &PluginFolder, name: &str, version: &str, on: bool) {
    let target = p.folder.join(name);
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("plugin.json"), manifest(name, version)).unwrap();
    std::fs::write(target.join("main.lua"), "return {}\n").unwrap();
    if on {
        p.set_enabled(&item(p, name), true).unwrap();
    }
}

fn zip_bytes(name: &str, version: &str) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file(format!("{name}/plugin.json"), options).unwrap();
        zip.write_all(manifest(name, version).as_bytes()).unwrap();
        zip.start_file(format!("{name}/main.lua"), options).unwrap();
        zip.write_all(b"return {}\n").unwrap();
        zip.finish().unwrap();
    }
    buf.into_inner()
}

/// GitHub with one release of extra, tagged tag, whose .zip is data.
#[derive(Default)]
struct GitHub {
    tag: String,
    data: Vec<u8>,
    offline: bool,
    corrupt: bool,
    offline_downloads: bool,
    asked: Mutex<Vec<String>>,
}

impl GitHub {
    fn new() -> GitHub {
        GitHub { tag: "v1.1".into(), data: zip_bytes("extra", "1.1"), ..Default::default() }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    fn zips_asked(&self) -> usize {
        self.asked().iter().filter(|u| u.ends_with("extra.zip")).count()
    }
}

impl Fetch for GitHub {
    fn get(&self, url: &str, _headers: &[(&str, &str)]) -> Result<Reply, String> {
        self.asked.lock().unwrap().push(url.into());
        if self.offline {
            return Err("offline".into());
        }
        let body = if url.starts_with("https://api.github.com/") {
            json!({"tag_name": self.tag, "html_url": format!("{REPO_URL}/releases/tag/{}", self.tag), "assets": [
                {"name": "extra.zip", "size": self.data.len(), "browser_download_url": format!("{REPO_URL}/x/extra.zip"),
                 "digest": format!("sha256:{}", sha256_hex(&self.data))}]}).to_string().into_bytes()
        } else if self.offline_downloads {
            return Err("offline".into());
        } else if self.corrupt {
            vec![b'x'; self.data.len()]
        } else {
            self.data.clone()
        };
        Ok(Reply { status: 200, headers: vec![], body: Box::new(std::io::Cursor::new(body)) })
    }
}

fn updater(p: &PluginFolder, github: &Arc<GitHub>) -> PluginUpdater {
    PluginUpdater::new(p.folder.parent().unwrap().join("plugin-updates.json"), github.clone())
}

fn check(p: &PluginFolder, u: &mut PluginUpdater) -> bool {
    u.maybe_check(p);
    u.wait(p)
}

fn item(p: &PluginFolder, name: &str) -> Installed {
    p.find(name).unwrap()
}

#[test]
fn a_newer_release_is_only_mentioned_by_default() {
    let p = plugins();
    install(&p, "extra", "1.0", true);
    let github = Arc::new(GitHub::new());
    let mut u = updater(&p, &github);
    check(&p, &mut u);
    assert_eq!(u.newer(&p, &item(&p, "extra")).unwrap().tag, "v1.1");
    assert!(item(&p, "extra").version == "1.0" && github.asked().len() == 1, "nothing downloaded");
}

#[test]
fn with_auto_update_on_a_plugin_thats_on_is_replaced_and_stays_on() {
    let p = plugins();
    install(&p, "extra", "1.0", true);
    p.switches.set_switch(AUTO_UPDATE, true).unwrap();
    let mut u = updater(&p, &Arc::new(GitHub::new()));
    check(&p, &mut u);
    let now = item(&p, "extra");
    assert!(now.version == "1.1" && now.enabled, "{now:?}");
    assert_eq!(u.updated.get("extra").map(String::as_str), Some("1.1"));
    assert!(u.newer(&p, &now).is_none());
}

#[test]
fn auto_update_leaves_a_plugin_thats_off_alone() {
    let p = plugins();
    install(&p, "extra", "1.0", false);
    p.switches.set_switch(AUTO_UPDATE, true).unwrap();
    let github = Arc::new(GitHub::new());
    let mut u = updater(&p, &github);
    check(&p, &mut u);
    assert!(item(&p, "extra").version == "1.0" && u.newer(&p, &item(&p, "extra")).is_some() && github.asked().len() == 1);
}

#[test]
fn auto_update_refuses_a_zip_holding_another_plugin() {
    let p = plugins();
    install(&p, "extra", "1.0", true);
    p.switches.set_switch(AUTO_UPDATE, true).unwrap();
    let mut u = updater(&p, &Arc::new(GitHub { data: zip_bytes("sneaky", "1.1"), ..GitHub::new() }));
    check(&p, &mut u);
    assert_eq!(p.find_installed().iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["extra"]);
    assert!(item(&p, "extra").version == "1.0" && item(&p, "extra").enabled && u.updated.is_empty());
}

#[test]
fn a_release_auto_update_tried_isnt_downloaded_again() {
    let p = plugins();
    install(&p, "extra", "1.0", true);
    p.switches.set_switch(AUTO_UPDATE, true).unwrap();
    let github = Arc::new(GitHub { data: zip_bytes("sneaky", "1.1"), ..GitHub::new() });
    check(&p, &mut updater(&p, &github));
    let mut u = updater(&p, &github);
    // Forget the check but not what was tried
    u.cache_mut().repos.clear();
    u.cache().save();
    check(&p, &mut u);
    assert_eq!(github.zips_asked(), 1);
}

#[test]
fn offline_nothing_happens_and_nothing_is_said() {
    let p = plugins();
    install(&p, "extra", "1.0", true);
    p.switches.set_switch(AUTO_UPDATE, true).unwrap();
    let mut u = updater(&p, &Arc::new(GitHub { offline: true, ..GitHub::new() }));
    let changed = check(&p, &mut u);
    assert!(u.newer(&p, &item(&p, "extra")).is_none() && item(&p, "extra").version == "1.0" && changed);
}

#[test]
fn with_the_check_off_github_isnt_asked() {
    let p = plugins();
    install(&p, "extra", "1.0", true);
    p.switches.set_switch(CHECK_UPDATES, false).unwrap();
    p.switches.set_switch(AUTO_UPDATE, true).unwrap();
    let github = Arc::new(GitHub::new());
    check(&p, &mut updater(&p, &github));
    assert!(github.asked().is_empty());
}

#[test]
fn a_download_that_fails_isnt_tried_again_every_hour() {
    let p = plugins();
    install(&p, "extra", "1.0", true);
    p.switches.set_switch(AUTO_UPDATE, true).unwrap();
    // Doesn't match its digest
    let github = Arc::new(GitHub { corrupt: true, ..GitHub::new() });
    let mut u = updater(&p, &github);
    check(&p, &mut u);
    // The next hourly tick, the release still cached as newer
    check(&p, &mut u);
    assert_eq!(github.zips_asked(), 1);
    assert!(item(&p, "extra").version == "1.0" && u.newer(&p, &item(&p, "extra")).unwrap().tag == "v1.1");
}

#[test]
fn a_download_that_failed_is_tried_again_after_a_restart() {
    let p = plugins();
    install(&p, "extra", "1.0", true);
    p.switches.set_switch(AUTO_UPDATE, true).unwrap();
    // The connection dropped mid-download
    check(&p, &mut updater(&p, &Arc::new(GitHub { offline_downloads: true, ..GitHub::new() })));
    assert_eq!(item(&p, "extra").version, "1.0");
    let mut u = updater(&p, &Arc::new(GitHub::new()));
    // A day later
    u.cache_mut().repos.clear();
    u.cache().save();
    check(&p, &mut u);
    assert_eq!(item(&p, "extra").version, "1.1");
}

#[test]
fn a_release_whose_zip_isnt_newer_isnt_installed() {
    let p = plugins();
    install(&p, "extra", "1.1", true);
    p.switches.set_switch(AUTO_UPDATE, true).unwrap();
    // Mislabelled
    let mut u = updater(&p, &Arc::new(GitHub { tag: "v1.2".into(), data: zip_bytes("extra", "1.0"), ..GitHub::new() }));
    check(&p, &mut u);
    let now = item(&p, "extra");
    assert!(now.version == "1.1" && now.enabled && u.updated.is_empty());
}
