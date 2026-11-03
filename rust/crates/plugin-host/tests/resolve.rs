//! A guest's files come back as plain paths Slint can import from, and nothing outside the plugin folder does.

use telescope_core::plugin::PluginInfo;
use telescope_core::testing::scratch_dir;
use telescope_plugin_host::PluginContext;

fn context(folder: &std::path::Path) -> PluginContext {
    PluginContext::new(PluginInfo { name: "x".into(), ..Default::default() }, folder, vec![])
}

#[test]
fn a_file_of_the_plugin_resolves_to_a_path_without_the_verbatim_prefix() {
    let folder = scratch_dir("resolve_plain");
    std::fs::write(folder.join("card.slint"), "export component A {}").unwrap();
    let path = context(&folder).resolve("card.slint").unwrap();
    assert!(!path.to_string_lossy().starts_with(r"\\?\"), "{path:?}");
    assert!(path.is_file());
}

#[test]
fn a_path_that_leads_outside_the_plugin_is_still_refused() {
    let folder = scratch_dir("resolve_outside");
    let plugin = folder.join("plugin");
    std::fs::create_dir(&plugin).unwrap();
    std::fs::write(folder.join("secret.txt"), "x").unwrap();
    let ctx = context(&plugin);
    assert!(ctx.resolve("../secret.txt").is_err());
    assert!(ctx.resolve("missing.slint").is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(folder.join("secret.txt"), plugin.join("link.txt")).unwrap();
        assert!(ctx.resolve("link.txt").unwrap_err().contains("outside"));
    }
}
