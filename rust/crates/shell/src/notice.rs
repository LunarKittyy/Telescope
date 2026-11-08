//! A message before any window exists (the QMessageBox main.py and single_instance.py show when a start can't go on): a title, the
//! words and an OK button, shown on its own until it's closed.

use crate::compile::compile;
use slint::ComponentHandle;
use slint_interpreter::{ComponentInstance, Value};
use telescope_core::i18n::tr;

const DOC: &str = r#"
import { Theme, DialogFrame, Button } from "@ui-kit/ui-kit.slint";
export component Notice inherits Window {
    in property <string> heading;
    in property <string> body;
    in property <string> ok;
    callback close;
    title: heading;
    background: Theme.ground;
    preferred-width: 480px;
    min-width: 360px;
    DialogFrame {
        title: heading;
        VerticalLayout {
            spacing: 16px;
            Text {
                text: body;
                wrap: word-wrap;
                font-family: Theme.font;
                font-size: Theme.body;
                color: Theme.ink;
                line-height-factor: Theme.line-factor(self.font-size);
            }
            HorizontalLayout {
                alignment: end;
                Button { text: ok; role: "primary"; clicked => { close(); } }
            }
        }
    }
}
"#;

/// The message's window, not shown yet. OK closes it.
pub fn build(title: &str, text: &str) -> Result<ComponentInstance, String> {
    let ui = compile("notice.slint", DOC, Vec::new(), "Notice")?.create().map_err(|e| e.to_string())?;
    let set = |name: &str, value: &str| ui.set_property(name, Value::String(value.into())).map_err(|e| e.to_string());
    set("heading", title)?;
    set("body", text)?;
    set("ok", &tr("OK"))?;
    let weak = ui.as_weak();
    ui.set_callback("close", move |_| {
        if let Some(ui) = weak.upgrade() {
            let _ = ui.hide();
        }
        Value::Void
    }).map_err(|e| e.to_string())?;
    Ok(ui)
}

/// Shows the message and waits until it's closed. An Err says why it couldn't be shown (no display, say).
pub fn show(title: &str, text: &str) -> Result<(), String> {
    let ui = build(title, text)?;
    ui.run().map_err(|e| e.to_string())
}
