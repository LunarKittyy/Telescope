// The folded pins' list stays inside the window and scrolls when its rows would not fit above the bar.
mod motion_film;

use motion_film::*;
use slint_interpreter::Value;
use telescope_shell::offscreen;

const DOC: &str = r#"
import { Footer } from "shell/frame.slint";
import { Theme } from "@ui-kit/ui-kit.slint";
export component T inherits Window {
    width: 600px;
    height: 300px;
    background: Theme.ground;
    in property <int> folded;
    out property <length> list-height: bar.list-height;
    out property <length> bar-y: bar.y;
    public function open() { bar.open-folded(0, 20px); }
    bar := Footer {
        x: 0;
        y: root.height - self.height;
        width: root.width;
        left-folded: root.folded;
    }
}
"#;

#[test]
fn a_list_of_many_folded_pins_is_no_taller_than_the_window_above_the_bar() {
    let _still = offscreen::freeze_clock();
    let ui = make(DOC, 600.0, 300.0);
    set(&ui, "folded", Value::Number(3.0));
    ui.invoke("open", &[]).unwrap();
    let few = num(&ui, "list-height");
    set(&ui, "folded", Value::Number(40.0));
    ui.invoke("open", &[]).unwrap();
    let many = num(&ui, "list-height");
    let bar = num(&ui, "bar-y");
    assert!(few > 0.0 && few < many, "a few pins take their own height: {few} then {many}");
    assert!(many <= bar, "forty pins fit above the bar at {bar}: {many}");
}
