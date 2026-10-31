//! A plugin from the plugins folder, loaded and run on the graphics card (test_plugin_loader.py's first test). Without a graphics
//! card the picture isn't checked.

use serde_json::json;
use std::path::Path;
use telescope_core::config::ConfigFile;
use telescope_core::frame::{Frame, FrameContext, FrameStage, PixelFormat};
use telescope_core::media::{Media, StagedStep};
use telescope_core::testing::{scratch_dir, FakeHost};
use telescope_plugin_host::package::PluginFolder;
use telescope_plugin_host::switches::Switches;

struct Ctx;

impl FrameContext for Ctx {
    fn source_id(&self) -> &str {
        ""
    }
    fn person_mask(&self, _frame: &Frame) -> Option<std::sync::Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() { copy_dir(&e.path(), &target) } else { std::fs::copy(e.path(), &target).unwrap(); }
    }
}

#[test]
fn a_plugin_is_off_until_switched_on_then_loads() {
    let root = scratch_dir("installed-plugin");
    let folder = root.join("plugins");
    std::fs::create_dir(&folder).unwrap();
    #[cfg(unix)]
    for path in [&root, &folder] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/plugin-host/example-plugins/black_and_white"), &folder.join("black_and_white"));
    let p = PluginFolder::new(&folder, Switches::new(ConfigFile::at(root.join("telescope_config.json"))), []);
    let [found] = &p.find_installed()[..] else { panic!("one plugin") };
    assert!(found.problem.is_empty() && !found.enabled && found.title == "Black and white", "{found:?}");
    let (loaded, failed) = p.load_enabled("en");
    assert!(loaded.is_empty() && failed.is_empty(), "nothing runs that wasn't switched on");
    p.set_enabled(found, true).unwrap();
    let (mut loaded, failed) = p.load_enabled("en");
    assert!(failed.is_empty() && loaded.len() == 1, "{failed:?}");
    let plugin = &mut loaded[0];
    let info = plugin.info();
    assert!(info.name == "black_and_white" && info.title == "Black and white" && info.about.starts_with("Turns the picture"));
    plugin.setup(&mut FakeHost::new());
    plugin.set_config(&json!({"on": true})).unwrap();
    let step = plugin.frame_step().expect("its frame step");
    if telescope_media::gpu::Gpu::shared().is_none() {
        eprintln!("no graphics card here: the picture isn't checked");
        return;
    }
    let mut pipeline = telescope_media::FfmpegMedia.pipeline().expect("a frame pipeline");
    pipeline.set_steps(vec![StagedStep { owner: "black_and_white".into(), stage: FrameStage::Effects, step }]);
    let red = Frame::new(4, 4, PixelFormat::Rgba, [200, 0, 0, 255].repeat(16));
    let out = pipeline.process(red, &Ctx);
    assert_eq!((out.width, out.height), (4, 4));
    assert!(out.data.chunks(4).all(|p| p[0] == p[2] && p[0] == p[1]), "{:?}", &out.data[..4]);
}
