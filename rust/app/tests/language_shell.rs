// The language that's loaded reaches every text: Slint's @tr (with and without a context) through the shell's compile, and the
// Rust tr() the plugins call. The catalogs are the real ones, as the Python app has them.
use slint::ComponentHandle;
use std::path::Path;
use telescope_core::i18n;

const CARD: &str = r#"
export component Test inherits Window {
    width: 300px;
    height: 200px;
    Text { text: @tr("Camera"); accessible-label: self.text; }
    Text { y: 40px; text: @tr("camera section" => "Light"); accessible-label: self.text; }
    Text { y: 80px; text: @tr("plugins" => "On"); accessible-label: self.text; }
    Text { y: 120px; text: @tr("Not a text of the app"); accessible-label: self.text; }
}
"#;

fn texts() -> Vec<String> {
    let def = telescope_shell::compile::compile("lang.slint", "import { Test } from \"card.slint\";\nexport { Test }", vec![("card.slint".into(), CARD.into())], "Test").unwrap();
    let ui = def.create().unwrap();
    ui.show().unwrap();
    i_slint_backend_testing::ElementHandle::find_by_element_type_name(&ui, "Text").filter_map(|e| e.accessible_label().map(|l| l.to_string())).collect()
}

#[test]
fn slint_and_rust_texts_follow_the_language_loaded() {
    i_slint_backend_testing::init_no_event_loop();
    i18n::use_folder(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../desktop/telescope/locale"));
    // English: what the code says
    assert_eq!(i18n::load(Some("en")), "en");
    let english = texts();
    assert_eq!(english, ["Camera", "Light", "On", "Not a text of the app"]);
    // Swedish: the same texts from the catalog, a context's text included
    assert_eq!(i18n::load(Some("sv")), "sv");
    let swedish = texts();
    assert_eq!(swedish, ["Kamera", "Ljus", "På", "Not a text of the app"]);
    assert_eq!(i18n::tr("Camera"), "Kamera");
    assert_eq!(i18n::tr_ctx("camera section", "Light"), "Ljus");
    // What the settings file picked is what start() loads
    std::env::remove_var(i18n::ENV_LANGUAGE);
    assert_eq!(i18n::start("de"), "de");
    assert_eq!(i18n::tr("Camera"), "Kamera");
    assert_eq!(i18n::start("en"), "en");
    assert_eq!(i18n::tr("Camera"), "Camera");
}
