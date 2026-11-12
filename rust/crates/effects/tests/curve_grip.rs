//! A curve point dragged in something that folds away stops changing the curve, and the editor is let go of: nothing keeps editing from a drag
//! the person can no longer see. Driven through the testing backend, which draws nothing.

use i_slint_backend_testing::mock_elapsed_time;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};
use slint_interpreter::{ComponentInstance, Value};
use std::path::Path;

fn spin_on<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

// The curve editor under a fold, the way the Colour grading card holds it
const TEST: &str = r#"
export component T inherits Window {
    width: 300px;
    height: 300px;
    in-out property <bool> open: true;
    in-out property <int> presses;
    in-out property <int> moves;
    in-out property <int> releases;
    Unfolding {
        open: root.open;
        snap: false;
        CurveBox {
            height: 120px;
            points: [{ x: 0.5, y: 0.5 }];
            pressed(x, y, w, h, right) => { presses += 1; }
            moved(x, y, w, h) => { moves += 1; }
            released => { releases += 1; }
        }
    }
}
"#;

fn make() -> ComponentInstance {
    i_slint_backend_testing::init_no_event_loop();
    let source = format!("{}\n{}", include_str!("../ui/parts.slint"), TEST);
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin_on(compiler.build_from_source(source, Path::new("/root/curve.slint").to_path_buf()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error).map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let ui = result.component("T").expect("the test window").create().unwrap();
    ui.window().set_size(slint::LogicalSize::new(300.0, 300.0));
    ui.show().unwrap();
    ui
}

fn num(ui: &ComponentInstance, name: &str) -> f64 {
    match ui.get_property(name).unwrap() {
        Value::Number(n) => n,
        v => panic!("{name}: {v:?}"),
    }
}

fn pointer(ui: &ComponentInstance, event: impl FnOnce(LogicalPosition) -> WindowEvent, x: f32, y: f32) {
    ui.window().dispatch_event(event(LogicalPosition::new(x, y)));
}

#[test]
fn a_curve_drag_in_a_fold_that_closes_changes_nothing_more_and_is_let_go() {
    let ui = make();
    mock_elapsed_time(std::time::Duration::from_millis(500));
    pointer(&ui, |position| WindowEvent::PointerMoved { position }, 100.0, 60.0);
    pointer(&ui, |position| WindowEvent::PointerPressed { position, button: PointerEventButton::Left }, 100.0, 60.0);
    for i in 1..4 {
        pointer(&ui, |position| WindowEvent::PointerMoved { position }, 100.0 + 5.0 * i as f32, 60.0);
        mock_elapsed_time(std::time::Duration::from_millis(16));
    }
    assert_eq!(num(&ui, "presses"), 1.0, "the press reaches the editor");
    assert!(num(&ui, "moves") > 0.0, "the drag moves the point while the fold is open");
    ui.set_property("open", Value::Bool(false)).unwrap();
    mock_elapsed_time(std::time::Duration::from_millis(16));
    assert_eq!(num(&ui, "releases"), 1.0, "the editor is let go of when the fold closes, the button still down");
    let before = num(&ui, "moves");
    for i in 4..8 {
        pointer(&ui, |position| WindowEvent::PointerMoved { position }, 100.0 + 5.0 * i as f32, 60.0);
        mock_elapsed_time(std::time::Duration::from_millis(16));
    }
    assert_eq!(num(&ui, "moves"), before, "and the captured moves change nothing");
    pointer(&ui, |position| WindowEvent::PointerReleased { position, button: PointerEventButton::Left }, 135.0, 60.0);
    assert_eq!(num(&ui, "releases"), 1.0, "and the button coming up lets go no second time");
}
