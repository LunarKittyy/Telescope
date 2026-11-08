// Layout rule (tests/test_text_fits.py): widths come from the layout and text has to fit inside them. Qt's fixed-width push buttons could
// clip a longer translation, which is what that file looks for; ui-kit's Button takes its minimum width from its own text up to a cap, past which
// a longer text ends in an ellipsis instead of being cut off or pushing its row out of its place. This checks that for the buttons every card and dialog uses.
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{ComponentHandle, ModelRc, VecModel};
use slint_interpreter::Value;
use telescope_core::plugin::Plugin;
use telescope_core::ui::SlintSource;

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
    ui.set_property("label", Value::String(text.into())).unwrap();
    ui.show().unwrap();
    let button = ElementHandle::find_by_element_type_name(&ui, "Button").next().expect("the button");
    (ui.window().size().width as f32, button.size().width)
}

#[test]
fn a_button_is_as_wide_as_its_text_whatever_the_language() {
    i_slint_backend_testing::init_no_event_loop();
    let short = widths("OK");
    let long = widths("Telescope can't start from inside the zip, so extract it first");
    // The button is no narrower than its text, even in a window narrower than that, up to a cap (Theme.u(70) = 140 px): a longer
    // label gives way past it and ends in an ellipsis, so one long translation never pushes a row out of its card
    assert!(long.1 > short.1 + 50.0, "{short:?} {long:?}");
    assert!(long.1 > long.0, "the window is only 60 wide: {long:?}");
    assert!(long.1 <= 140.5, "a long label stops at the cap: {long:?}");
    // The pseudo language, which makes every text 40% longer to find a layout with no room for a translation
    let pseudo = widths(&telescope_core::i18n::pseudo("Telescope can't start from inside the zip, so extract it first"));
    assert!((pseudo.1 - long.1).abs() < 1.0, "{long:?} {pseudo:?}");
}

struct Never;

impl telescope_core::system::Autostart for Never {
    fn enabled(&self) -> bool {
        false
    }
    fn set_enabled(&self, _on: bool, _minimized: bool) -> Result<(), String> {
        Ok(())
    }
}

/// How wide this text wants to be in the body font.
fn body_width(text: &str) -> f32 {
    let doc = r#"import { Theme } from "@ui-kit/ui-kit.slint";
export component Measure inherits Window {
    in property <string> says;
    out property <length> wide: measured.preferred-width;
    measured := Text { text: root.says; font-family: Theme.font; font-size: Theme.body; }
}"#;
    let def = telescope_shell::compile::compile("measure.slint", doc, vec![], "Measure").unwrap();
    let ui = def.create().unwrap();
    ui.set_property("says", Value::String(text.into())).unwrap();
    match ui.get_property("wide").unwrap() {
        Value::Number(n) => n as f32,
        other => panic!("{other:?}"),
    }
}

// test_text_fits.py::test_automatic_streaming_fields_fit: the wait's number and its longest unit show whole, in English and in the pseudo language
#[test]
fn automatic_streaming_fields_fit() {
    i_slint_backend_testing::init_no_event_loop();
    let mut plugin = telescope_system::startup::StartupPlugin::new(std::sync::Arc::new(Never));
    let section = plugin.settings_sections().into_iter().nth(1).expect("the Automatic streaming section");
    let SlintSource::Text(source) = section.ui.source.clone() else { panic!("source text") };
    let def = telescope_shell::compile::compile("startup.slint", &source, vec![], &section.ui.component).unwrap();
    let units: Vec<String> = section.ui.props["unit-items"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(units.len(), 2);
    let pseudo = units.iter().map(|u| telescope_core::i18n::pseudo(u));
    for unit in units.iter().cloned().chain(pseudo) {
        let ui = def.create().unwrap();
        let items: Vec<Value> = vec![Value::String(unit.as_str().into())];
        ui.set_property("unit-items", Value::Model(ModelRc::new(VecModel::from(items)))).unwrap();
        ui.set_property("unit-current", Value::Number(0.0)).unwrap();
        ui.set_property("delay-title", Value::String("Wait".into())).unwrap();
        ui.show().unwrap();
        let shown = ElementHandle::find_by_accessible_label(&ui, &unit).find(|e| e.accessible_role() == Some(AccessibleRole::Text)).expect("the unit's text");
        assert!(shown.size().width >= body_width(&unit), "{unit:?} gets {} of {}", shown.size().width, body_width(&unit));
        // The field's text sits inside 13px on each side; the longest wait is 3600 seconds
        let value = ElementHandle::find_by_accessible_label(&ui, "Wait").find(|e| e.accessible_role() == Some(AccessibleRole::TextInput)).expect("the wait's number");
        assert!(value.size().width - 26.0 >= body_width("3600"), "{} wide", value.size().width);
    }
}
