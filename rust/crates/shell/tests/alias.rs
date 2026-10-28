// Checks the Slint features the generated window leans on: aliases to a child's in-properties and callbacks, changed handlers.
use slint_interpreter::{Compiler, Value};

#[test]
fn aliases_reach_a_childs_properties_and_callbacks() {
    i_slint_backend_testing::init_no_event_loop();
    let src = r#"
component Child inherits Rectangle {
    in property <int> a;
    in-out property <bool> b;
    callback said(string, float);
    out property <int> doubled: a * 2;
}
export component W inherits Window {
    c := Child { }
    in-out property p-a <=> c.a;
    in-out property p-b <=> c.b;
    callback p-said <=> c.said;
    out property <int> seen: c.doubled;
    in-out property <int> changes;
    changed seen => { changes += 1; }
    public function fire() { c.said("x", 2.5); }
}
"#;
    let r = spin_on(Compiler::default().build_from_source(src.into(), Default::default()));
    for d in r.diagnostics() { eprintln!("{d}"); }
    let w = r.component("W").unwrap().create().unwrap();
    w.set_property("p-a", Value::Number(21.0)).unwrap();
    assert_eq!(w.get_property("seen").unwrap(), Value::Number(42.0));
    let got = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let g = got.clone();
    w.set_callback("p-said", move |args| { g.borrow_mut().extend_from_slice(args); Value::Void }).unwrap();
    w.invoke("fire", &[]).unwrap();
    assert_eq!(got.borrow().len(), 2);
}

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
