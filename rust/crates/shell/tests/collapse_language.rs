// A fold is remembered in any language: the key is the English the card was written with, whatever its title shows.
use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_shell::collapse::Collapsing;

const CARD: &str = r#"
import { Card, Section, Folds, Icons } from "@ui-kit/ui-kit.slint";
export { Folds }
export component Test inherits Window {
    width: 400px;
    height: 600px;
    out property <length> light-height: light.height;
    Card {
        title: @tr("Things");
        icon: Icons.sun;
        fold-key: "camera/Things";
        light := Section { scope: "camera/Things"; key: "Light"; title: @tr("Light"); icon: Icons.sun; Text { text: "row"; } }
    }
}
"#;

#[test]
fn a_fold_is_remembered_in_any_language() {
    i_slint_backend_testing::init_no_event_loop();
    telescope_core::i18n::set_translator(|m| match m {
        "Things" => Some("Saker".into()),
        "Light" => Some("Ljus".into()),
        _ => None,
    });
    // Texts are put in the person's language as a file is loaded, so the card is a file of its own
    let doc = "import { Test } from \"card.slint\";\nexport { Test }\nexport { Folds } from \"card.slint\";";
    let def = telescope_shell::compile::compile("language.slint", doc, vec![("card.slint".into(), CARD.into())], "Test").unwrap();
    let ui = def.create().unwrap();
    let mut folds = Collapsing::default();
    folds.load(Some(&serde_json::json!(["camera/Things/Light"]))); // what the English app saved
    let state = std::rc::Rc::new(std::cell::RefCell::new(folds));
    let s = state.clone();
    ui.set_global_callback("Folds", "is-folded", move |args| match args.first() {
        Some(Value::String(key)) => Value::Bool(s.borrow().is_folded(key)),
        _ => Value::Bool(false),
    }).unwrap();
    ui.show().unwrap();
    // The Swedish titles show, and the section is folded all the same: no room is left for its row
    let titles: Vec<String> = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Ljus").filter_map(|e| e.accessible_label().map(|l| l.to_string())).collect();
    assert!(!titles.is_empty(), "the translated title is there");
    let folded = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&ui, "Ljus").find(|e| e.accessible_checkable() == Some(true)).unwrap();
    assert_eq!(folded.accessible_checked(), Some(false));
    telescope_core::i18n::set_translator(|_| None);
}
