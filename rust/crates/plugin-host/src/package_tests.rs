//! Installing, switching on and loading plugins that don't come with Telescope (test_plugin_loader.py).

use super::*;
use crate::switches::{Switches, AUTO_UPDATE};
use serde_json::json;
use std::io::Write;
use telescope_core::config::ConfigFile;
use telescope_core::testing::{scratch_dir, FakeHost};
use telescope_core::FrameStep;

pub(crate) const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/example-plugins/black_and_white");

/// A plugins folder (755, as a user's would be) with its own settings file.
pub(crate) fn plugins() -> PluginFolder {
    let root = scratch_dir("plugins");
    let folder = root.join("plugins");
    fs::create_dir(&folder).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    }
    PluginFolder::new(folder, Switches::new(ConfigFile::at(root.join("telescope_config.json"))), [])
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() { copy_dir(&e.path(), &target) } else { fs::copy(e.path(), &target).unwrap(); }
    }
}

/// The Black and white example copied in as name.
pub(crate) fn example(folder: &Path, name: &str) -> PathBuf {
    let target = folder.join(name);
    copy_dir(Path::new(EXAMPLE), &target);
    if name != "black_and_white" {
        let mut m: Value = serde_json::from_str(&fs::read_to_string(target.join(MANIFEST)).unwrap()).unwrap();
        m["name"] = json!(name);
        fs::write(target.join(MANIFEST), m.to_string()).unwrap();
    }
    target
}

const SIMPLE: &str = "return {}\n";

/// A plugin folder with a main.lua and a plugin.json with extra keys.
pub(crate) fn plugin(folder: &Path, name: &str, code: &str, extra: Value) -> PathBuf {
    let target = folder.join(name);
    fs::create_dir(&target).unwrap();
    let mut manifest = json!({"name": name, "api": 2});
    for (k, v) in extra.as_object().into_iter().flatten() {
        manifest[k] = v.clone();
    }
    fs::write(target.join(MANIFEST), manifest.to_string()).unwrap();
    fs::write(target.join("main.lua"), code).unwrap();
    target
}

fn one(p: &PluginFolder) -> Installed {
    let mut all = p.find_installed();
    assert_eq!(all.len(), 1, "{all:?}");
    all.remove(0)
}

fn names(loaded: &[Box<dyn telescope_core::Plugin>]) -> Vec<String> {
    loaded.iter().map(|p| p.info().name).collect()
}

#[test]
fn a_plugin_is_off_until_switched_on_then_loads() {
    let p = plugins();
    example(&p.folder, "black_and_white");
    let found = one(&p);
    assert!(found.problem.is_empty() && !found.enabled && found.title == "Black and white", "{found:?}");
    let (loaded, failed) = p.load_enabled("en");
    assert!(loaded.is_empty() && failed.is_empty(), "nothing runs that wasn't switched on");
    p.set_enabled(&found, true).unwrap();
    let (mut loaded, failed) = p.load_enabled("en");
    assert!(failed.is_empty() && names(&loaded) == ["black_and_white"], "{failed:?}");
    let plugin = &mut loaded[0];
    let info = plugin.info();
    assert!(info.title == "Black and white" && info.about.starts_with("Turns the picture"));
    plugin.setup(&mut FakeHost::new());
    plugin.set_config(&json!({"on": true})).unwrap();
    let Some(FrameStep::Shader(step)) = plugin.frame_step() else { panic!("no shader") };
    assert!(step.wgsl.contains("fn effect") && step.uniforms == json!({"strength": 1.0}));
}

#[test]
fn a_plugin_that_changed_after_it_was_switched_on_stays_off() {
    let p = plugins();
    let folder = example(&p.folder, "black_and_white");
    p.set_enabled(&one(&p), true).unwrap();
    let main = folder.join("main.lua");
    fs::write(&main, fs::read_to_string(&main).unwrap() + "\n-- changed\n").unwrap();
    let found = one(&p);
    assert!(!found.enabled && found.changed);
    let (loaded, failed) = p.load_enabled("en");
    assert!(loaded.is_empty() && failed.len() == 1 && failed[0].error.contains("changed"));
}

#[test]
fn a_manifest_that_doesnt_fit_says_why() {
    for (extra, expect) in [(json!({"name": "other"}), "folder's name"), (json!({"api": 99}), "newer Telescope"), (json!({"api": 0}), "older Telescope"),
                            (json!({"api": 1}), "plugin API 1"), (json!({"main": "../x.lua"}), "isn't valid"), (json!({"main": "other.lua"}), "has no other.lua")] {
        let p = plugins();
        plugin(&p.folder, "thing", SIMPLE, extra.clone());
        let problem = one(&p).problem;
        assert!(problem.contains(expect), "{extra}: {problem}");
    }
}

#[test]
fn a_python_plugin_says_it_needs_an_update() {
    let p = plugins();
    let folder = p.folder.join("old_one");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join(MANIFEST), r#"{"name": "old_one", "api": 1, "class": "P"}"#).unwrap();
    fs::write(folder.join("__init__.py"), "").unwrap();
    assert_eq!(one(&p).problem, "It was made for an older Telescope (plugin API 1) and needs an update.");
}

#[test]
fn a_built_in_name_cant_be_taken() {
    let mut p = plugins();
    p.reserved.insert("camera_control".into());
    plugin(&p.folder, "camera_control", SIMPLE, json!({}));
    assert!(one(&p).problem.contains("comes with Telescope"));
}

#[test]
fn a_folder_without_a_manifest_or_with_a_bad_name_is_listed_with_why() {
    let p = plugins();
    fs::create_dir(p.folder.join("no_manifest")).unwrap();
    fs::create_dir(p.folder.join("Bad-Name")).unwrap();
    let problems: BTreeMap<String, String> = p.find_installed().into_iter().map(|i| (i.name, i.problem)).collect();
    assert!(problems["no_manifest"].contains("plugin.json") && problems["Bad-Name"].contains("lowercase"));
}

use std::collections::BTreeMap;

#[cfg(unix)]
#[test]
fn a_link_inside_a_plugin_is_refused() {
    let p = plugins();
    let folder = plugin(&p.folder, "thing", SIMPLE, json!({}));
    let elsewhere = p.folder.parent().unwrap().join("elsewhere.lua");
    fs::write(&elsewhere, "").unwrap();
    std::os::unix::fs::symlink(&elsewhere, folder.join("helper.lua")).unwrap();
    assert!(one(&p).problem.contains("link"));
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_plugin_other_users_can_change_isnt_loaded() {
    let p = plugins();
    let folder = plugin(&p.folder, "thing", SIMPLE, json!({}));
    chmod(&folder, 0o777);
    assert!(one(&p).problem.contains("Other users"));
}

#[cfg(unix)]
#[test]
fn group_write_is_fine_when_the_group_is_the_users_own() {
    let p = plugins();
    let folder = plugin(&p.folder, "thing", SIMPLE, json!({}));
    chmod(&folder, 0o775);
    PRIVATE_GROUP.with(|g| g.set(Some(true)));
    assert_eq!(one(&p).problem, "");
    PRIVATE_GROUP.with(|g| g.set(Some(false)));
    assert!(one(&p).problem.contains("Other users"), "a group others are in");
    PRIVATE_GROUP.with(|g| g.set(None));
}

#[test]
fn compiled_code_never_runs() {
    let p = plugins();
    let folder = plugin(&p.folder, "thing", SIMPLE, json!({}));
    let bytecode = mlua::Lua::new().load("return {}").into_function().unwrap().dump(false);
    fs::write(folder.join("main.lua"), bytecode).unwrap();
    p.set_enabled(&one(&p), true).unwrap();
    let (loaded, failed) = p.load_enabled("en");
    assert!(loaded.is_empty() && failed[0].error.contains("main.lua"), "{:?}", failed[0].error);
    // Nor from inside: load() only takes text, and string.dump is gone
    fs::write(folder.join("main.lua"), "local f, err = load(string.char(27) .. 'Lua')\nassert(f == nil and err:find('binary'), err)\n\
        assert(string.dump == nil)\nreturn {}\n").unwrap();
    p.set_enabled(&one(&p), true).unwrap();
    let (loaded, failed) = p.load_enabled("en");
    assert!(failed.is_empty() && loaded.len() == 1, "{failed:?}");
}

#[test]
fn compiled_files_next_to_the_source_are_refused() {
    for name in ["helper.luac", "native.so", "lib.dll", "helper.pyc"] {
        let p = plugins();
        let folder = plugin(&p.folder, "thing", SIMPLE, json!({}));
        fs::write(folder.join(name), b"\0").unwrap();
        assert!(one(&p).problem.contains("compiled code"), "{name}");
    }
}

#[test]
fn a_python_plugin_says_its_too_old_not_that_its_compiled() {
    let p = plugins();
    let folder = plugin(&p.folder, "thing", SIMPLE, json!({"api": 1}));
    fs::create_dir(folder.join("__pycache__")).unwrap();
    fs::write(folder.join("__pycache__/__init__.cpython-312.pyc"), b"\0").unwrap();
    assert!(one(&p).problem.contains("older Telescope"), "{}", one(&p).problem);
}

#[test]
fn one_that_was_on_and_got_a_bad_file_says_why_it_didnt_start() {
    let p = plugins();
    let folder = plugin(&p.folder, "thing", SIMPLE, json!({}));
    p.set_enabled(&one(&p), true).unwrap();
    fs::write(folder.join("native.so"), b"\0").unwrap();
    let (loaded, failed) = p.load_enabled("en");
    assert!(loaded.is_empty() && failed[0].error.contains("compiled code"), "so the window shows a banner");
}

#[cfg(unix)]
#[test]
fn a_file_inside_other_users_can_change_is_caught() {
    let p = plugins();
    let folder = plugin(&p.folder, "thing", SIMPLE, json!({}));
    fs::create_dir(folder.join("sub")).unwrap();
    fs::write(folder.join("sub/x.lua"), "").unwrap();
    chmod(&folder.join("sub/x.lua"), 0o666);
    assert!(one(&p).problem.contains("Other users"));
}

#[test]
fn a_plugin_that_tries_to_exit_only_fails_itself() {
    let p = plugins();
    plugin(&p.folder, "thing", "os.exit(3)\nreturn {}\n", json!({}));
    p.set_enabled(&one(&p), true).unwrap();
    let (loaded, failed) = p.load_enabled("en");
    assert!(loaded.is_empty() && failed[0].error.contains("exit"), "{}", failed[0].error);
}

#[test]
fn a_plugins_own_modules_load_and_nothing_else_does() {
    let p = plugins();
    let folder = plugin(&p.folder, "thing", "local helper = require('lib.helper')\nlocal again = require('lib.helper')\n\
        assert(helper == again)\nreturn { value = function() return helper.VALUE, json.encode({1}) end }\n", json!({}));
    fs::create_dir(folder.join("lib")).unwrap();
    fs::write(folder.join("lib/helper.lua"), "return { VALUE = 42 }\n").unwrap();
    fs::write(folder.join("json.lua"), "error('a planted json module ran')\n").unwrap();
    p.set_enabled(&one(&p), true).unwrap();
    let plugin = crate::load::load(&one(&p), "en").unwrap();
    assert_eq!(plugin.call(None, "value", vec![]).unwrap(), json!(42));
    for bad in ["../outside", "/etc/passwd", "lib/../../x"] {
        let code = format!("return {{ setup = function() require({bad:?}) end }}\n");
        fs::write(folder.join("main.lua"), code).unwrap();
        let mut t = crate::testing::PluginTest::open(&folder).unwrap();
        assert!(t.call("setup", vec![]).unwrap_err().contains("module name"), "{bad}");
    }
}

#[test]
fn a_plugin_that_raises_on_load_is_reported_not_raised() {
    let p = plugins();
    plugin(&p.folder, "thing", "error('broken on purpose')\n", json!({}));
    p.set_enabled(&one(&p), true).unwrap();
    let (loaded, failed) = p.load_enabled("en");
    assert!(loaded.is_empty() && failed[0].error.contains("broken on purpose"), "{}", failed[0].error);
    assert!(failed[0].error.contains("main.lua:1"), "it says where: {}", failed[0].error);
}

#[test]
fn a_main_that_doesnt_return_a_table_is_refused() {
    let p = plugins();
    plugin(&p.folder, "thing", "local plugin = {}\n", json!({}));
    p.set_enabled(&one(&p), true).unwrap();
    let (_, failed) = p.load_enabled("en");
    assert!(failed[0].error.contains("return the plugin's table"), "{}", failed[0].error);
}

// ── Installing from a .zip ──────────────────────────────────────────────────

pub(crate) fn write_zip(path: &Path, files: &[(&str, Vec<u8>)], links: &[&str]) -> PathBuf {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in files {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    for name in links {
        zip.add_symlink(*name, "/etc/passwd", options).unwrap();
    }
    zip.finish().unwrap();
    path.to_path_buf()
}

/// The example's files under prefix, as a .zip would hold them.
pub(crate) fn example_files(prefix: &str) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let name = format!("{rel}{}", e.file_name().to_string_lossy());
            if e.file_type().unwrap().is_dir() { walk(&e.path(), &format!("{name}/"), out) } else { out.push((name, fs::read(e.path()).unwrap())) }
        }
    }
    let mut out = Vec::new();
    walk(Path::new(EXAMPLE), prefix, &mut out);
    out.sort();
    if prefix != "black_and_white/" {
        let name = prefix.trim_end_matches('/');
        for (n, data) in &mut out {
            if n.ends_with(MANIFEST) {
                let mut m: Value = serde_json::from_slice(data).unwrap();
                m["name"] = json!(name);
                *data = m.to_string().into_bytes();
            }
        }
    }
    out
}

fn zip_of(path: &Path, files: &[(String, Vec<u8>)]) -> PathBuf {
    let refs: Vec<(&str, Vec<u8>)> = files.iter().map(|(n, d)| (n.as_str(), d.clone())).collect();
    write_zip(path, &refs, &[])
}

fn scratch(p: &PluginFolder) -> PathBuf {
    p.folder.parent().unwrap().to_path_buf()
}

fn listing(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn a_zip_installs_switched_off() {
    let p = plugins();
    let archive = zip_of(&scratch(&p).join("bw.zip"), &example_files("black_and_white/"));
    let found = p.install_zip(&archive, false).unwrap();
    assert!(found.problem.is_empty() && !found.enabled && p.folder.join("black_and_white/plugin.json").is_file(), "{found:?}");
    assert_eq!(listing(&p.folder), ["black_and_white"], "nothing left from unpacking");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        for rel in files(&p.folder.join("black_and_white"), false).unwrap() {
            assert_eq!(fs::metadata(p.folder.join("black_and_white").join(rel)).unwrap().mode() & 0o022, 0);
        }
    }
}

#[test]
fn a_zip_that_could_write_elsewhere_is_refused() {
    // The files in the zip, its folders, and a word of the refusal
    type Case<'a> = (&'a [(&'a str, &'a [u8])], &'a [&'a str], &'a str);
    let cases: &[Case] = &[
        (&[("black_and_white/../../evil.lua", b"")], &[], "outside"),
        (&[("/abs/evil.lua", b"")], &[], "outside"),
        (&[("black_and_white\\..\\evil.lua", b"")], &[], "outside"),
        (&[("C:/evil.lua", b"")], &[], "outside"),
        (&[("a/x.lua", b""), ("b/y.lua", b"")], &[], "one folder"),
        (&[("Black And White/x.lua", b"")], &[], "lowercase"),
        (&[("black_and_white/x.lua", b"")], &["black_and_white/link"], "link"),
    ];
    for (files, links, expect) in cases {
        let p = plugins();
        let owned: Vec<(&str, Vec<u8>)> = files.iter().map(|(n, d)| (*n, d.to_vec())).collect();
        let archive = write_zip(&scratch(&p).join("bad.zip"), &owned, links);
        match p.install_zip(&archive, false) {
            Err(InstallError::Invalid(why)) => assert!(why.contains(expect), "{files:?}: {why}"),
            other => panic!("{files:?}: {other:?}"),
        }
        assert!(listing(&p.folder).is_empty() && !scratch(&p).join("evil.lua").exists());
    }
}

#[test]
fn a_zip_too_big_is_refused() {
    let p = plugins();
    let archive = write_zip(&scratch(&p).join("big.zip"), &[("black_and_white/plugin.json", b"{}".to_vec()),
                                                             ("black_and_white/big.bin", vec![0; MAX_BYTES as usize + 1])], &[]);
    assert!(matches!(p.install_zip(&archive, false), Err(InstallError::Invalid(why)) if why.contains("too big")));
}

#[test]
fn a_zip_thats_not_a_zip_is_refused() {
    let p = plugins();
    let path = scratch(&p).join("x.zip");
    fs::write(&path, b"nope").unwrap();
    assert!(matches!(p.install_zip(&path, false), Err(InstallError::Invalid(why)) if why.contains("isn't a .zip")));
}

#[test]
fn installing_over_one_already_there_needs_replace() {
    let p = plugins();
    example(&p.folder, "black_and_white");
    let archive = zip_of(&scratch(&p).join("bw.zip"), &example_files("black_and_white/"));
    assert!(matches!(p.install_zip(&archive, false), Err(InstallError::Exists(name)) if name == "black_and_white"));
}

fn newer_example(p: &PluginFolder, version: &str) -> PathBuf {
    let mut files = example_files("black_and_white/");
    for (name, data) in &mut files {
        if name.ends_with(MANIFEST) {
            let mut m: Value = serde_json::from_slice(data).unwrap();
            m["version"] = json!(version);
            *data = m.to_string().into_bytes();
        }
    }
    files.push(("black_and_white/extra.lua".into(), b"return 1\n".to_vec()));
    zip_of(&scratch(p).join("bw2.zip"), &files)
}

#[test]
fn replacing_keeps_the_settings_and_switches_it_off() {
    let p = plugins();
    example(&p.folder, "black_and_white");
    fs::write(p.folder.join("black_and_white/old_only.lua"), "").unwrap();
    p.set_enabled(&one(&p), true).unwrap();
    assert!(p.switches.file.update(|cfg| {
        cfg["plugin_configs"]["black_and_white"] = json!({"on": true});
        cfg["devices"] = json!({"phone": {"plugin_configs": {"black_and_white": {"on": true}}}});
    }));
    let found = p.install_zip(&newer_example(&p, "2.0"), true).unwrap();
    assert!(found.version == "2.0" && found.problem.is_empty() && !found.enabled && !found.changed, "{found:?}");
    assert!(p.folder.join("black_and_white/extra.lua").is_file());
    assert!(!p.folder.join("black_and_white/old_only.lua").exists(), "replaced, not merged");
    assert_eq!(listing(&p.folder), ["black_and_white"], "nothing left from the swap");
    let cfg = p.switches.file.load();
    assert_eq!(cfg["plugin_configs"]["black_and_white"], json!({"on": true}));
    assert_eq!(cfg["devices"]["phone"]["plugin_configs"]["black_and_white"], json!({"on": true}));
    let (loaded, failed) = p.load_enabled("en");
    assert!(loaded.is_empty() && failed.is_empty(), "off, not \"changed since switched on\"");
}

#[test]
fn replacing_an_identical_copy_still_switches_it_off() {
    let p = plugins();
    example(&p.folder, "black_and_white");
    p.set_enabled(&one(&p), true).unwrap();
    let archive = zip_of(&scratch(&p).join("bw.zip"), &example_files("black_and_white/"));
    assert!(!p.install_zip(&archive, true).unwrap().enabled);
}

#[test]
fn replacing_leaves_the_update_switches_alone() {
    let p = plugins();
    example(&p.folder, "black_and_white");
    p.switches.set_switch(AUTO_UPDATE, true).unwrap();
    p.set_enabled(&one(&p), true).unwrap();
    p.install_zip(&newer_example(&p, "2.0"), true).unwrap();
    assert!(p.switches.switch(AUTO_UPDATE, false));
}

#[test]
fn a_zip_with_another_plugin_installs_next_to_it() {
    let p = plugins();
    example(&p.folder, "black_and_white");
    p.set_enabled(&one(&p), true).unwrap();
    let found = p.install_zip(&zip_of(&scratch(&p).join("other.zip"), &example_files("other_one/")), true).unwrap();
    assert_eq!(found.name, "other_one");
    let installed: BTreeMap<String, Installed> = p.find_installed().into_iter().map(|i| (i.name.clone(), i)).collect();
    assert_eq!(installed.keys().collect::<Vec<_>>(), ["black_and_white", "other_one"]);
    assert!(installed["black_and_white"].enabled);
}

#[test]
fn a_bad_zip_leaves_the_installed_one_alone() {
    let p = plugins();
    example(&p.folder, "black_and_white");
    p.set_enabled(&one(&p), true).unwrap();
    let archive = write_zip(&scratch(&p).join("bad.zip"), &[("black_and_white/../evil.lua", vec![])], &[]);
    assert!(matches!(p.install_zip(&archive, true), Err(InstallError::Invalid(why)) if why.contains("outside")));
    assert!(one(&p).enabled);
}

#[cfg(unix)]
#[test]
fn replacing_a_link_is_refused() {
    let p = plugins();
    let real = example(&scratch(&p), "black_and_white");
    std::os::unix::fs::symlink(&real, p.folder.join("black_and_white")).unwrap();
    let archive = zip_of(&scratch(&p).join("bw.zip"), &example_files("black_and_white/"));
    assert!(matches!(p.install_zip(&archive, true), Err(InstallError::Invalid(why)) if why.contains("isn't a folder")));
    assert!(real.join(MANIFEST).is_file());
}

#[test]
fn the_zips_plugin_name_can_be_read_first() {
    let dir = scratch_dir("zipname");
    assert_eq!(zip_plugin_name(&zip_of(&dir.join("bw.zip"), &example_files("black_and_white/"))).unwrap(), "black_and_white");
    let two = write_zip(&dir.join("two.zip"), &[("a/x.lua", vec![]), ("b/y.lua", vec![])], &[]);
    assert!(matches!(zip_plugin_name(&two), Err(InstallError::Invalid(why)) if why.contains("one folder")));
}

#[test]
fn repo_is_shown_only_when_its_a_plain_https_address() {
    let long = format!("https://x.org/{}", "a".repeat(300));
    let cases = [
        (json!("https://github.com/someone/their_plugin"), "https://github.com/someone/their_plugin"),
        (json!("https://codeberg.org/someone/plugin/"), "https://codeberg.org/someone/plugin/"),
        (Value::Null, ""),
        (json!("http://github.com/someone/plugin"), ""),
        (json!("javascript:alert(1)"), ""),
        (json!("https://github.com/a b"), ""),
        (json!("https://x.org/\"><b>hi</b>"), ""),
        (json!(long), ""),
        (json!(["https://x.org"]), ""),
    ];
    for (repo, shown) in cases {
        let p = plugins();
        let extra = if repo.is_null() { json!({}) } else { json!({"repo": repo}) };
        plugin(&p.folder, "extra", SIMPLE, extra);
        let found = one(&p);
        assert!(found.repo == shown && found.problem.is_empty(), "{repo}: a bad one never keeps it from loading");
        assert_eq!(!manifest::repo_problem(Some(&repo)).is_empty(), !repo.is_null() && shown.is_empty(), "{repo}");
    }
}

#[test]
fn a_built_in_name_cant_be_installed() {
    let mut p = plugins();
    p.reserved.insert("presets".into());
    let archive = zip_of(&scratch(&p).join("p.zip"), &example_files("presets/"));
    assert!(matches!(p.install_zip(&archive, false), Err(InstallError::Invalid(why)) if why.contains("comes with Telescope")));
}

#[test]
fn removing_deletes_the_folder_and_its_switch() {
    let p = plugins();
    example(&p.folder, "black_and_white");
    p.set_enabled(&one(&p), true).unwrap();
    p.remove(&one(&p)).unwrap();
    assert!(listing(&p.folder).is_empty() && p.switches.added().is_empty());
}

#[test]
fn removing_refuses_a_folder_outside_the_plugins_folder() {
    let p = plugins();
    let outside = scratch(&p).join("keep_me");
    fs::create_dir(&outside).unwrap();
    let fake = Installed { folder: outside.clone(), name: "keep_me".into(), ..Default::default() };
    assert!(p.remove(&fake).is_err());
    assert!(outside.is_dir());
}

#[test]
fn a_plugin_being_written_loads_from_its_folder_and_stays_on_when_it_changes() {
    let p = plugins();
    let work = scratch(&p).join("work");
    let folder = example(&work, "black_and_white");
    fs::create_dir(folder.join(".git")).unwrap();
    fs::write(folder.join(".git/HEAD"), "ref").unwrap();
    fs::write(folder.join(".git/packed.so"), "a repository around it doesn't count as part of the plugin").unwrap();
    let p = p.with_dev_folders(vec![folder.clone()]);
    let found = one(&p);
    assert!(found.dev && found.enabled && found.problem.is_empty() && found.folder == folder, "{found:?}");
    let main = folder.join("main.lua");
    fs::write(&main, fs::read_to_string(&main).unwrap() + "\n-- changed\n").unwrap();
    let (loaded, failed) = p.load_enabled("en");
    assert!(failed.is_empty() && names(&loaded) == ["black_and_white"]);
}

#[test]
fn a_plugin_being_written_stands_in_for_an_installed_copy() {
    let p = plugins();
    example(&p.folder, "black_and_white");
    let folder = example(&scratch(&p).join("work"), "black_and_white");
    let p = p.with_dev_folders(vec![folder.clone()]);
    let found = one(&p);
    assert!(found.dev && found.folder == folder);
}

#[test]
fn a_plugin_being_written_that_cant_load_says_why() {
    let p = plugins();
    let work = scratch(&p).join("work");
    fs::create_dir(&work).unwrap();
    let broken = plugin(&work, "broken", "error('no module named shiny', 0)\n", json!({}));
    let wrong = plugin(&work, "wrong", SIMPLE, json!({"api": 99}));
    let p = p.with_dev_folders(vec![broken, wrong]);
    let (loaded, failed) = p.load_enabled("en");
    assert!(loaded.is_empty());
    let errors: BTreeMap<String, String> = failed.into_iter().map(|f| (f.name, f.error)).collect();
    assert_eq!(errors["broken"], "no module named shiny");
    assert_eq!(errors["wrong"], "It needs a newer Telescope (plugin API 99; this one has 2).");
}

#[test]
fn plugins_being_written_need_a_dev_profile() {
    let written = example(&scratch_dir("work"), "black_and_white");
    // The only test that touches these variables
    // SAFETY: no other test in this crate reads or writes TELESCOPE_DEV_PLUGINS, and none relies on TELESCOPE_DEV_PROFILE being unset
    unsafe {
        std::env::set_var(DEV_PLUGINS_ENV, &written);
        std::env::remove_var(telescope_core::config::DEV_PROFILE_ENV);
    }
    let p = plugins().with_dev_folders(dev_plugin_folders());
    assert!(p.find_installed().is_empty());
    let (loaded, failed) = p.load_enabled("en");
    assert!(loaded.is_empty() && failed.is_empty());
    unsafe { std::env::set_var(telescope_core::config::DEV_PROFILE_ENV, scratch_dir("profile")) };
    assert_eq!(dev_plugin_folders(), [written]);
    assert_eq!(names(&plugins().with_dev_folders(dev_plugin_folders()).load_enabled("en").0), ["black_and_white"]);
    unsafe {
        std::env::remove_var(telescope_core::config::DEV_PROFILE_ENV);
        std::env::remove_var(DEV_PLUGINS_ENV);
    }
}

#[test]
fn a_swap_that_fails_leaves_the_old_one_switched_on() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let p = plugins().with_rename(move |from, to| {
        // The new folder into place, as when Windows has a file in it open
        if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"));
        }
        fs::rename(from, to)
    });
    example(&p.folder, "black_and_white");
    p.set_enabled(&one(&p), true).unwrap();
    assert!(matches!(p.install_zip(&newer_example(&p, "2.0"), true), Err(InstallError::Io(_))));
    let found = one(&p);
    assert!(found.version == "1.0" && found.enabled, "{found:?}");
    assert_eq!(listing(&p.folder), ["black_and_white"]);
}
