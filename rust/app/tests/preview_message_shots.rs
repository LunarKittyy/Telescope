use slint::ComponentHandle;
use slint_interpreter::Value;
use telescope_shell::offscreen;

const DOC: &str = r#"
import { PreviewMessage } from "shell/preview.slint";
export { Theme } from "@ui-kit/ui-kit.slint";
export component PreviewMessageShot inherits Window {
    in property <bool> message-visible: true;
    in property <bool> counting: true;
    in property <bool> dim: true;
    background: @linear-gradient(180deg, #101728 0%, #302b36 100%);
    if root.message-visible: PreviewMessage {
        width: parent.width;
        height: parent.height;
        text: "Taking a picture";
        detail: "Video will come back when done.";
        step: "Shot 3 of 8";
        countdown: root.counting ? "24 s left" : "";
        progress: 0.58;
        counting: root.counting;
        dim: root.dim;
    }
}
"#;

fn output_dir() -> Option<std::path::PathBuf> {
    let dir = std::env::var("PREVIEW_MESSAGE_SHOTS").or_else(|_| std::env::var("CARD_SHOTS")).ok()?;
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    Some(dir)
}

#[test]
fn preview_message_draws_at_wide_and_narrow_sizes() {
    offscreen::install();
    let def = telescope_shell::compile::compile("preview-message-shot.slint", DOC, vec![], "PreviewMessageShot").expect("the preview message compiles");
    let dir = output_dir();
    for width in [960.0, 340.0] {
        let ui = def.create().unwrap();
        ui.window().set_size(slint::LogicalSize::new(width, 600.0));
        ui.show().unwrap();
        offscreen::pump();
        let picture = offscreen::draw(ui.window(), width, 600.0, 1.0).expect("a preview message picture");
        assert_eq!((picture.width, picture.height), (width as u32, 600));
        let white = picture.rgba.as_chunks::<4>().0.iter().filter(|p| p[0] > 235 && p[1] > 235 && p[2] > 235).count();
        assert!(white > width as usize, "the title, countdown and progress fill draw at {width}");
        if let Some(dir) = &dir {
            picture.save_png(&dir.join(format!("preview-message-{}.png", width as u32))).unwrap();
        }
        ui.set_property("message-visible", Value::Bool(false)).unwrap();
        let clear = offscreen::draw(ui.window(), width, 600.0, 1.0).expect("the clear preview");
        assert_ne!(picture.rgba, clear.rgba, "the message changes the preview");
        assert!(picture.rgba[0] < clear.rgba[0], "the scrim dims the picture's corner");
        ui.set_property("message-visible", Value::Bool(true)).unwrap();
        ui.set_property("counting", Value::Bool(false)).unwrap();
        ui.set_property("dim", Value::Bool(false)).unwrap();
        let no_countdown = offscreen::draw(ui.window(), width, 600.0, 1.0).expect("a message without a countdown");
        assert_ne!(picture.rgba, no_countdown.rgba);
        assert_eq!(&no_countdown.rgba[..4], &clear.rgba[..4], "a message can leave the picture undimmed");
        ui.hide().unwrap();
    }
}
