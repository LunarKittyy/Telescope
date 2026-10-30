// Layout rule (tests/test_text_fits.py): widths come from the layout and text has to fit inside them. Qt's fixed-width push buttons could
// clip a longer translation, which is what that file looks for; ui-kit's Button takes its minimum width from its own text, so a longer
// text widens the button and what holds it instead of being cut off. This checks that for the buttons every card and dialog uses.
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;

const DOC: &str = r#"
import { Button } from "@ui-kit/ui-kit.slint";
export component Test inherits Window {
    in property <string> label;
    width: 60px;
    height: 80px;
    HorizontalLayout {
        Button { text: root.label; }
    }
}
"#;

/// The window's width and the button's, for this text.
fn widths(text: &str) -> (f32, f32) {
    let def = telescope_shell::compile::compile("fits.slint", "import { Test } from \"t.slint\";\nexport { Test }", vec![("t.slint".into(), DOC.into())], "Test").unwrap();
    let ui = def.create().unwrap();
    ui.set_property("label", slint_interpreter::Value::String(text.into())).unwrap();
    ui.show().unwrap();
    let button = ElementHandle::find_by_element_type_name(&ui, "Button").next().expect("the button");
    (ui.window().size().width as f32, button.size().width)
}

#[test]
fn a_button_is_as_wide_as_its_text_whatever_the_language() {
    i_slint_backend_testing::init_no_event_loop();
    let short = widths("OK");
    let long = widths("Telescope can't start from inside the zip, so extract it first");
    // The button is no narrower than its text, even in a window narrower than that
    assert!(long.1 > short.1 + 150.0, "{short:?} {long:?}");
    assert!(long.1 > long.0, "the window is only 60 wide: {long:?}");
    // The pseudo language, which makes every text 40% longer to find a layout with no room for a translation
    let pseudo = widths(&telescope_core::i18n::pseudo("Telescope can't start from inside the zip, so extract it first"));
    assert!(pseudo.1 > long.1, "{long:?} {pseudo:?}");
}
