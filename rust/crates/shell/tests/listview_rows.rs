// A ListView's content height follows rows added or removed beyond the rows it shows, also from a row's init (the interpreter skips instantiation walks when nothing they read changed, vendor/slint-interpreter/PATCH.txt).
use slint::platform::WindowEvent;
use slint::{ComponentHandle, LogicalPosition, LogicalSize, Model, VecModel};
use slint_interpreter::{ComponentInstance, Value};
use std::rc::Rc;

const DOC: &str = r#"
import { ListView } from "std-widgets.slint";
export component Test inherits Window {
    in-out property <[int]> rows;
    in property <[int]> other-rows;
    in property <int> swap-at: -1;
    in property <int> append-at: -1;
    callback append();
    out property <length> content-height: list.viewport-height;
    list := ListView {
        for row[index] in rows: Rectangle {
            height: row >= 1000 ? 40px : 20px;
            init => {
                if (index == root.swap-at) { root.rows = root.other-rows; }
                if (index == root.append-at) { root.append(); }
            }
        }
    }
}
"#;

fn numbers(n: usize) -> Rc<VecModel<Value>> {
    Rc::new(VecModel::from((0..n).map(|i| Value::Number(i as f64)).collect::<Vec<_>>()))
}

fn list(rows: &Rc<VecModel<Value>>) -> ComponentInstance {
    i_slint_backend_testing::init_no_event_loop();
    let def = telescope_shell::compile::compile("listview.slint", DOC, vec![], "Test").expect("the document compiles");
    let ui = def.create().unwrap();
    ui.window().set_size(LogicalSize::new(200.0, 200.0));
    ui.set_property("rows", Value::Model(rows.clone().into())).unwrap();
    ui.show().unwrap();
    ui
}

// Every input event runs the instantiation walk first
fn walk(ui: &ComponentInstance) {
    for _ in 0..2 {
        ui.window().dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(5.0, 5.0) });
    }
}

fn height(ui: &ComponentInstance) -> f64 {
    match ui.get_property("content-height").unwrap() {
        Value::Number(h) => h,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_list_view_grows_and_shrinks_for_rows_it_doesnt_show() {
    let model = numbers(100);
    let ui = list(&model);
    walk(&ui);
    assert_eq!(height(&ui), 2000.0);
    model.push(Value::Number(100.0));
    walk(&ui);
    assert_eq!(height(&ui), 2020.0, "a row added at the end");
    model.remove(model.row_count() - 1);
    model.remove(model.row_count() - 1);
    walk(&ui);
    assert_eq!(height(&ui), 1980.0, "two rows removed at the end");
}

#[test]
fn a_model_swapped_by_a_rows_init_is_followed() {
    let first = numbers(100);
    let other = Rc::new(VecModel::from(vec![Value::Number(1000.0); 30]));
    let ui = list(&first);
    ui.set_property("other-rows", Value::Model(other.clone().into())).unwrap();
    walk(&ui);
    // The window grows to show row 12, whose init swaps the model
    ui.set_property("swap-at", Value::Number(12.0)).unwrap();
    ui.window().set_size(LogicalSize::new(200.0, 300.0));
    walk(&ui);
    assert_eq!(height(&ui), 1200.0, "the swapped in model's rows");
    other.set_row_data(0, Value::Number(0.0));
    walk(&ui);
    // The ListView estimates rows it doesn't show from the ones it does, so a shorter row 0 shrinks it by more than 20px
    assert!(height(&ui) < 1200.0, "a row of the swapped in model changed: {}", height(&ui));
}

#[test]
fn a_row_added_by_a_rows_init_is_counted() {
    let model = numbers(100);
    let ui = list(&model);
    walk(&ui);
    assert_eq!(height(&ui), 2000.0);
    let m = model.clone();
    ui.set_callback("append", move |_| {
        m.push(Value::Number(0.0));
        Value::Void
    }).unwrap();
    // The window grows to show row 12, whose init adds a row beyond the ones shown
    ui.set_property("append-at", Value::Number(12.0)).unwrap();
    ui.window().set_size(LogicalSize::new(200.0, 300.0));
    walk(&ui);
    assert_eq!(height(&ui), 2020.0);
}
