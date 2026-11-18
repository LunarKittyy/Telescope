// Checks the patched Slint interpreter's shared expressions and item tables: edits through the internal API, and a property read while a thread ends.
use i_slint_compiler::llr::{Expression, MutExpression};
use slint_interpreter::{Compiler, ComponentDefinition, ComponentInstance, SharedString, Value};
use std::cell::RefCell;

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

fn build(src: &str) -> slint_interpreter::CompilationResult {
    let mut compiler = Compiler::default();
    compiler.set_style("fluent".into());
    let result = spin_on(compiler.build_from_source(src.into(), Default::default()));
    assert!(!result.has_errors(), "{:?}", result.diagnostics().collect::<Vec<_>>());
    result
}

fn foo() -> Value {
    Value::from(SharedString::from("foo"))
}

#[test]
fn a_body_edited_through_the_internal_api_reaches_new_instances() {
    i_slint_backend_testing::init_no_event_loop();
    let result = build(
        r#"
export component W inherits Window {
    public function answer() -> int { return 1; }
}
"#,
    );
    let definition: ComponentDefinition = result.component("W").unwrap();
    let before = definition.create().unwrap();
    assert_eq!(before.invoke("answer", &[]).unwrap(), Value::Number(1.0));
    let unit = result.compilation_unit(i_slint_core::InternalToken).unwrap();
    let mut edited = 0;
    for sub in unit.sub_components.iter() {
        for function in sub.functions.iter().filter(|f| f.name == "answer") {
            *function.code.borrow_mut() = Expression::NumberLiteral(2.0);
            edited += 1;
        }
    }
    assert_eq!(edited, 1);
    let after = definition.create().unwrap();
    assert_eq!(after.invoke("answer", &[]).unwrap(), Value::Number(2.0));
}

// A callback that edits the function it was called from must not hit a borrowed body.
#[test]
fn a_function_can_be_replaced_while_it_runs() {
    i_slint_backend_testing::init_no_event_loop();
    let result = build(
        r#"
export component W inherits Window {
    callback replace();
    public function answer() -> int { replace(); return 1; }
}
"#,
    );
    let definition = result.component("W").unwrap();
    let instance = definition.create().unwrap();
    let unit = result.compilation_unit(i_slint_core::InternalToken).unwrap();
    let code: *const MutExpression = unit
        .sub_components
        .iter()
        .flat_map(|sub| sub.functions.iter())
        .find(|f| f.name == "answer")
        .map(|f| &f.code)
        .unwrap();
    // The callback has to be 'static; `result` outlives the call below and owns the unit.
    let code = code as usize;
    instance
        .set_callback("replace", move |_| {
            let code = unsafe { &*(code as *const MutExpression) };
            *code.borrow_mut() = Expression::NumberLiteral(2.0);
            Value::Void
        })
        .unwrap();
    assert_eq!(instance.invoke("answer", &[]).unwrap(), Value::Number(1.0));
}

// The item tables' thread-local is destroyed before a thread-local that was created earlier, whose destructor reads a property of a native item.
#[test]
fn a_native_property_can_be_read_while_a_thread_ends() {
    struct ReadOnDrop(RefCell<Option<ComponentInstance>>);
    impl Drop for ReadOnDrop {
        fn drop(&mut self) {
            let instance = self.0.take().expect("instance");
            // The binding reads the text of a native item again, which looks its property up by name.
            instance.set_property("suffix", Value::from(SharedString::from("!"))).unwrap();
            assert_eq!(instance.get_property("shown").unwrap(), Value::from(SharedString::from("foo!")));
        }
    }
    thread_local! {
        static LAST_TO_GO: ReadOnDrop = const { ReadOnDrop(RefCell::new(None)) };
    }

    std::thread::spawn(|| {
        i_slint_backend_testing::init_no_event_loop();
        LAST_TO_GO.with(|_| ());
        let result = build(
            r#"
export component W inherits Window {
    in-out property<string> suffix;
    out property<string> shown: input.text + suffix;
    input := TextInput { text: "foo"; }
}
"#,
        );
        let instance = result.component("W").unwrap().create().unwrap();
        assert_eq!(instance.get_property("shown").unwrap(), foo());
        LAST_TO_GO.with(|last| *last.0.borrow_mut() = Some(instance));
    })
    .join()
    .unwrap();
}
