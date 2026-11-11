// A ListView's content height follows rows added or removed beyond the rows it shows (the interpreter skips instantiation walks when nothing they read changed, vendor/slint-interpreter/PATCH.txt).
use slint::platform::WindowEvent;
use slint::{ComponentHandle, LogicalPosition, Model, VecModel};
use slint_interpreter::Value;
use std::rc::Rc;

const DOC: &str = r#"
import { ListView } from "std-widgets.slint";
export component Test inherits Window {
    width: 200px;
    height: 200px;
    in property <[int]> rows;
    out property <length> content-height: list.viewport-height;
    list := ListView {
        for row in rows: Rectangle { height: 20px; }
    }
}
"#;

#[test]
fn a_list_view_grows_and_shrinks_for_rows_it_doesnt_show() {
    i_slint_backend_testing::init_no_event_loop();
    let def = telescope_shell::compile::compile("listview.slint", DOC, vec![], "Test").expect("the document compiles");
    let ui = def.create().unwrap();
    let model = Rc::new(VecModel::from((0..100).map(|i| Value::Number(i as f64)).collect::<Vec<_>>()));
    ui.set_property("rows", Value::Model(model.clone().into())).unwrap();
    ui.show().unwrap();
    // Every input event runs the instantiation walk first
    let walk = || ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(5.0, 5.0) });
    let height = || match ui.get_property("content-height").unwrap() {
        Value::Number(h) => h,
        other => panic!("{other:?}"),
    };
    walk();
    walk();
    assert_eq!(height(), 2000.0);
    model.push(Value::Number(100.0));
    walk();
    assert_eq!(height(), 2020.0, "a row added at the end");
    model.remove(model.row_count() - 1);
    model.remove(model.row_count() - 1);
    walk();
    assert_eq!(height(), 1980.0, "two rows removed at the end");
}
