// Checks the patched Slint interpreter's layout evaluation: cells from static items and repeaters, rebuilt when the repeater's model changes.
use i_slint_core::model::{ModelRc, VecModel};
use i_slint_core::api::LogicalPosition;
use i_slint_core::platform::WindowEvent;
use slint_interpreter::{Compiler, ComponentHandle, ComponentInstance, Value};

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

fn window() -> ComponentInstance {
    i_slint_backend_testing::init_no_event_loop();
    let mut compiler = Compiler::default();
    compiler.set_style("fluent".into());
    let result = spin_on(compiler.build_from_source(
        r#"
export component W inherits Window {
    width: 100px;
    height: 300px;
    in property <[int]> rows;
    out property <length> column-height: column.preferred-height;
    out property <length> last-y: last.y;
    out property <length> row-b-x: row-b.x;
    out property <length> row-b-width: row-b.width;
    column := VerticalLayout {
        spacing: 2px;
        Rectangle { height: 10px; }
        for h in rows: Rectangle { height: h * 1px; }
        last := Rectangle { height: 7px; }
    }
    HorizontalLayout {
        spacing: 4px;
        y: 200px;
        height: 20px;
        row-a := Rectangle { min-width: 10px; horizontal-stretch: 1; }
        for w in rows: Rectangle { preferred-width: w * 10px; horizontal-stretch: 0; }
        row-b := Rectangle { min-width: 6px; horizontal-stretch: 2; }
    }
}
"#
        .into(),
        Default::default(),
    ));
    assert!(!result.has_errors(), "{:?}", result.diagnostics().collect::<Vec<_>>());
    let instance = result.component("W").unwrap().create().unwrap();
    instance.show().unwrap();
    instance
}

fn rows(instance: &ComponentInstance, values: &[f64]) {
    let model = VecModel::from(values.iter().map(|v| Value::Number(*v)).collect::<Vec<_>>());
    instance.set_property("rows", Value::Model(ModelRc::new(model))).unwrap();
    // An input event makes the interpreter instantiate the repeater's rows
    instance
        .window()
        .dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(1.0, 1.0) });
}

fn number(instance: &ComponentInstance, name: &str) -> f64 {
    match instance.get_property(name).unwrap() {
        Value::Number(n) => n,
        other => panic!("{name} is {other:?}"),
    }
}

fn close(instance: &ComponentInstance, name: &str, want: f64) {
    let got = number(instance, name);
    assert!((got - want).abs() < 0.01, "{name}: {got}, wanted {want}");
}

#[test]
fn a_layout_follows_its_static_cells_and_its_repeater() {
    let w = window();
    // Without rows: 10 + 2 + 7, and the two stretching cells share what is left of 100 (6 + 10 + 4 of it fixed).
    close(&w, "column-height", 19.0);
    close(&w, "last-y", 12.0);
    close(&w, "row-b-x", 100.0 - (6.0 + 80.0 * 2.0 / 3.0));
    rows(&w, &[3.0, 5.0]);
    // 10 + 3 + 5 + 7 and three gaps of 2
    close(&w, "column-height", 31.0);
    close(&w, "last-y", 10.0 + 2.0 + 3.0 + 2.0 + 5.0 + 2.0);
    // Cells of 30 and 50 wide join the row: 10 + 30 + 50 + 6 + three gaps of 4 = 108 is over 100, so the stretching cells stay at their minimum.
    close(&w, "row-b-width", 6.0);
    rows(&w, &[1.0]);
    close(&w, "column-height", 10.0 + 1.0 + 7.0 + 4.0);
    // 10 + 10 + 6 + 2 gaps of 4 = 34 of 100: 66 left, a gets a third and b two thirds.
    close(&w, "row-b-width", 6.0 + 66.0 * 2.0 / 3.0);
    rows(&w, &[]);
    close(&w, "last-y", 12.0);
}
