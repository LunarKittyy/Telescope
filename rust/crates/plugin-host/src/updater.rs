//! The update check for added plugins as the window runs it (widgets/plugin_updater.py): a while after start, then hourly to see whether a
//! day has passed. With the auto-update switch on it also installs a newer release of a plugin that's on and switches it on again, from the
//! next start. The asking and downloading happen on a thread; installing happens in poll(), on the caller's thread.

use crate::package::{zip_plugin_name, PluginFolder};
use crate::switches::{AUTO_UPDATE, CHECK_UPDATES};
use crate::updates::{self, Cache, Fetch, Release};
use crate::Installed;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// After the app's own update check, and well clear of starting up.
pub const FIRST_CHECK: Duration = Duration::from_secs(30);
/// How often the window asks maybe_check() after that.
pub const TICK: Duration = Duration::from_secs(60 * 60);

/// A downloaded release: plugin name, its .zip and the release's tag.
type Download = (String, PathBuf, String);

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The version the plugin.json in a downloaded .zip gives, "" when it can't be read.
fn zip_version(path: &Path, name: &str) -> String {
    let read = || -> Option<String> {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
        let mut entry = zip.by_name(&format!("{name}/{}", crate::manifest::MANIFEST)).ok()?;
        let mut text = String::new();
        std::io::Read::read_to_string(&mut entry, &mut text).ok()?;
        let manifest: Value = serde_json::from_str(&text).ok()?;
        Some(manifest.get("version")?.as_str()?.to_string())
    };
    read().unwrap_or_default()
}

/// Makes `base/name` and nothing else, for its owner only: it fails when something is already there, so a folder somebody else laid out
/// first is never used (mkdtemp).
fn private_dir_in(base: &Path, name: &str) -> std::io::Result<PathBuf> {
    let dir = base.join(name);
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir)?;
    Ok(dir)
}

/// A new, empty, private folder under the temp folder (tempfile.mkdtemp(prefix="telescope-plugin-")).
fn scratch_folder() -> std::io::Result<PathBuf> {
    let random = ring::rand::SystemRandom::new();
    let mut last = None;
    for _ in 0..16 {
        let mut bytes = [0u8; 8];
        ring::rand::SecureRandom::fill(&random, &mut bytes).map_err(|_| std::io::Error::other("no random numbers"))?;
        let suffix: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        match private_dir_in(&std::env::temp_dir(), &format!("telescope-plugin-{suffix}")) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            other => return other,
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free folder name")))
}

pub struct PluginUpdater {
    fetch: Arc<dyn Fetch>,
    cache: Cache,
    thread: Option<JoinHandle<(Cache, Vec<Download>)>>,
    /// Plugin name: the version auto-update installed this run.
    pub updated: BTreeMap<String, String>,
    /// Plugin name: the release tag whose download failed this run (the next start tries again).
    failed: Arc<Mutex<HashMap<String, String>>>,
}

impl PluginUpdater {
    /// The app's: plugin-updates.json next to the settings, over HTTPS.
    pub fn app() -> PluginUpdater {
        PluginUpdater::new(updates::cache_path(), Arc::new(updates::Https))
    }

    pub fn new(cache_path: impl Into<PathBuf>, fetch: Arc<dyn Fetch>) -> PluginUpdater {
        PluginUpdater { fetch, cache: Cache::open(cache_path), thread: None, updated: BTreeMap::new(), failed: Arc::default() }
    }

    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    pub fn cache_mut(&mut self) -> &mut Cache {
        &mut self.cache
    }

    pub fn checking(&self, folder: &PluginFolder) -> bool {
        folder.switches.switch(CHECK_UPDATES, true)
    }

    pub fn auto(&self, folder: &PluginFolder) -> bool {
        self.checking(folder) && folder.switches.switch(AUTO_UPDATE, false)
    }

    pub fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// The release newer than the installed item that the last check found, or None.
    pub fn newer(&self, folder: &PluginFolder, item: &Installed) -> Option<Release> {
        let repo = updates::github_repo(&item.repo)?;
        if !self.checking(folder) {
            return None;
        }
        self.cache.release(&repo).filter(|r| updates::is_newer(&r.tag, &item.version))
    }

    /// Checks the repos that are due, in the background; quiet whatever happens.
    pub fn maybe_check(&mut self, folder: &PluginFolder) {
        if !self.checking(folder) || self.thread.is_some() {
            return;
        }
        let installed: Vec<Installed> = folder.find_installed().into_iter().filter(|i| !i.dev).collect();
        let repos: Vec<String> = installed.iter().filter_map(|i| updates::github_repo(&i.repo)).collect();
        if repos.is_empty() {
            return;
        }
        let auto: Vec<(String, String, String)> = if self.auto(folder) {
            installed.iter().filter(|i| i.enabled && i.problem.is_empty())
                .filter_map(|i| Some((i.name.clone(), updates::github_repo(&i.repo)?, i.version.clone()))).collect()
        } else {
            Vec::new()
        };
        let fetch = Arc::clone(&self.fetch);
        let failed = Arc::clone(&self.failed);
        let path = self.cache.path.clone();
        let spawned = std::thread::Builder::new().name("plugin-updates".into()).spawn(move || {
            let mut cache = Cache::open(path);
            updates::check(&repos, &mut cache, now(), fetch.as_ref());
            let mut downloads = Vec::new();
            for (name, repo, installed) in auto {
                let Some(release) = cache.release(&repo) else { continue };
                let tried = cache.auto_installed(&name) == Some(release.tag.as_str())
                    || failed.lock().unwrap_or_else(|e| e.into_inner()).get(&name) == Some(&release.tag);
                if !updates::is_newer(&release.tag, &installed) || tried {
                    continue;
                }
                let Some(asset) = release.zip_for(&name) else {
                    log::info!("The latest release of {repo} has no .zip to update {name} from");
                    continue;
                };
                let Ok(dir) = scratch_folder() else { continue };
                match updates::download(asset, &dir, fetch.as_ref()) {
                    Ok(zip) => downloads.push((name, zip, release.tag.clone())),
                    Err(e) => {
                        log::info!("Couldn't update {name}: {e}");
                        let _ = std::fs::remove_dir_all(&dir);
                        // Not downloaded again every hour, but a network blip isn't for good
                        failed.lock().unwrap_or_else(|e| e.into_inner()).insert(name, release.tag.clone());
                    }
                }
            }
            (cache, downloads)
        });
        match spawned {
            Ok(handle) => self.thread = Some(handle),
            Err(e) => log::warn!("Couldn't start the plugin update check: {e}"),
        }
    }

    /// Finishes a check that's done: takes what it found and installs what auto-update downloaded. True when one finished (the Plugins
    /// tab then shows what's newer).
    pub fn poll(&mut self, folder: &PluginFolder) -> bool {
        if self.thread.as_ref().is_none_or(|t| !t.is_finished()) {
            return false;
        }
        self.finish(folder)
    }

    /// For tests: waits for a running check, then poll()s.
    pub fn wait(&mut self, folder: &PluginFolder) -> bool {
        if self.thread.is_none() {
            return false;
        }
        self.finish(folder)
    }

    fn finish(&mut self, folder: &PluginFolder) -> bool {
        let Some(handle) = self.thread.take() else { return false };
        let Ok((cache, downloads)) = handle.join() else { return true };
        self.cache = cache;
        let any = !downloads.is_empty();
        for (name, zip, tag) in downloads {
            self.install(folder, &name, &zip, &tag);
            if let Some(dir) = zip.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        if any {
            self.cache.save();
        }
        true
    }

    /// Puts a downloaded update in place and switches it on, if auto-update is still on and so is the plugin.
    fn install(&mut self, folder: &PluginFolder, name: &str, zip: &Path, tag: &str) {
        // Tried once: a release that doesn't install isn't fetched daily
        self.cache.auto_installed.insert(name.to_string(), Value::String(tag.to_string()));
        let Some(current) = folder.find(name) else { return };
        if !self.auto(folder) || current.dev || !current.enabled {
            return;
        }
        let result = (|| -> Result<(), String> {
            if zip_plugin_name(zip).map_err(|e| e.to_string())? != name {
                return Err(format!("its .zip holds another plugin, not {name}"));
            }
            let version = zip_version(zip, name);
            if !updates::is_newer(&version, &current.version) {
                return Err(format!("its .zip is version {}, not newer than {}", or_q(&version), or_q(&current.version)));
            }
            let item = folder.install_zip(zip, true).map_err(|e| e.to_string())?;
            if item.problem.is_empty() {
                folder.set_enabled(&item, true).map_err(|e| e.to_string())?;
                log::info!("Updated {name} to {tag} by itself");
            } else {
                log::warn!("Updated {name} to {tag}, but it can't load: {}", item.problem);
            }
            self.updated.insert(name.to_string(), if item.version.is_empty() { tag.to_string() } else { item.version });
            Ok(())
        })();
        if let Err(e) = result {
            log::warn!("Couldn't update {name} to {tag}: {e}");
        }
    }
}

fn or_q(s: &str) -> &str {
    if s.is_empty() { "?" } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("telescope-updater-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_folder_somebody_else_made_first_is_not_used() {
        let base = base("taken");
        std::fs::create_dir(base.join("telescope-plugin-1")).unwrap();
        assert!(private_dir_in(&base, "telescope-plugin-1").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn the_scratch_folder_is_for_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let base = base("mode");
        let dir = private_dir_in(&base, "telescope-plugin-2").unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scratch_folders_are_new_each_time_and_not_guessable_from_the_pid() {
        let (a, b) = (scratch_folder().unwrap(), scratch_folder().unwrap());
        assert_ne!(a, b);
        let guess = std::env::temp_dir().join(format!("telescope-plugin-{}-0", std::process::id()));
        assert!(a != guess && b != guess);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
}
