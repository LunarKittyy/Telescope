// The Plugins tab with the Lab tag on the 3D row and the Limited tag on Privacy blur, drawn offscreen. Set LAB_BADGE_OUT to a folder to keep the picture.
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_shell::scenes::{Page, Scene, Setup};

struct Info(PluginInfo);

impl Plugin for Info {
    fn info(&self) -> PluginInfo {
        self.0.clone()
    }
}

// The JSON entries as they are, not the real plugins, so the picture shows what builtins.json says
fn from_json(_: &telescope_core::BusSender) -> Vec<Box<dyn Plugin>> {
    let infos: Vec<PluginInfo> = serde_json::from_str(include_str!("../data/builtins.json")).unwrap();
    // Only the streaming group, so the 3D row and its neighbours fit in the picture
    infos.into_iter().filter(|i| i.category == "streaming" || i.category == "vision" || i.required).map(|i| Box::new(Info(i)) as Box<dyn Plugin>).collect()
}

fn shot(name: &str) {
    let out = std::env::var_os("LAB_BADGE_OUT").map(std::path::PathBuf::from).unwrap_or_else(|| telescope_core::testing::scratch_dir("lab-badge"));
    let scenes = vec![Scene::new(name, || Setup {
        plugins: Some(from_json),
        page: Page::Settings("plugins".into()),
        every_optional: false,
        ..Setup::window(None, serde_json::json!({}), 940.0, 1000.0)
    })];
    let (n, failed) = telescope_shell::scenes::render(&scenes, &[1.0], &out).expect("rendered");
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(n, 1);
}

#[test]
fn the_plugins_tab_draws_with_the_lab_and_limited_tags() {
    shot("plugins-tab-2");
}

#[test]
fn the_plugins_tab_draws_with_the_tags_in_the_light_look() {
    std::env::set_var("SCENES_LOOK", "light");
    shot("plugins-tab-2-light");
}
