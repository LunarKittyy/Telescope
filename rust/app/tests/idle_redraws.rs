// A window with nothing changing on it asks for no frames: the owner's monitor runs at 180 Hz, so a redraw that nothing needed is
// paid for in full, and a window that is only sitting there should cost nothing. Counts the windows' redraw requests over ten
// seconds of clock on the real window with real cards, folded and open, with Settings open and with a stream up.
#![cfg(target_os = "linux")]
mod rig;

use rig::Rig;
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use telescope_core::ui::SlintSource;
use telescope_core::plugin::Plugin;
use telescope_shell::offscreen;

const SECONDS: u32 = 10;

fn cards(link: &std::sync::Arc<rig::FakeLink>, heard: &rig::Log) -> Vec<Box<dyn Plugin>> {
    let _ = link;
    vec![
        Box::new(rig::Listener::new("panel", heard)),
        Box::new(telescope_effects::transforms::TransformsPlugin::new()),
        Box::new(telescope_effects::colour_grading::ColourGradingPlugin::new()),
        Box::new(telescope_effects::cinema::CinemaPlugin::new()),
        Box::new(telescope_effects::fun_pack::FunPackPlugin::new()),
        Box::new(telescope_effects::privacy_blur::PrivacyBlurPlugin::new()),
        Box::new(telescope_effects::desk::DeskPlugin::new()),
        Box::new(telescope_effects::name_tag::NameTagPlugin::new()),
        Box::new(telescope_effects::notes::NotesPlugin::new()),
        Box::new(telescope_setup::monitoring::MonitoringPlugin::new()),
        Box::new(telescope_setup::stats::StatsPlugin::new()),
    ]
}

/// Lets the animations a change started finish, drawing as the window would.
fn settle(r: &Rig) {
    for _ in 0..40 {
        r.skip(50);
        offscreen::redraw(r.w.ui.window());
    }
}

/// The clock moved on for `SECONDS`, with the timers and the host's own once-a-second pass the way a running app has them; how many
/// times `window` asked to be drawn again.
fn redraws_over(r: &Rig, window: &slint::Window) -> u32 {
    let mut count = 0;
    for step in 0..SECONDS * 10 {
        r.skip(100);
        if step % 10 == 0 {
            r.w.process();
        }
        if offscreen::redraw(window) {
            count += 1;
        }
    }
    count
}

fn up(name: &str) -> Rig {
    let r = Rig::with(name, cards);
    r.w.ui.window().set_size(slint::LogicalSize::new(1300.0, 900.0));
    r.w.ui.show().unwrap();
    r.w.process();
    settle(&r);
    r
}

#[test]
fn a_window_nothing_changes_on_draws_nothing() {
    let r = up("idle-redraws");
    assert_eq!(redraws_over(&r, r.w.ui.window()), 0);
}

/// The titles of the cards that fold.
fn fold_buttons(r: &Rig) -> Vec<i_slint_backend_testing::ElementHandle> {
    let titles: Vec<String> = {
        let st = r.w.state.borrow();
        st.cards().into_iter().filter_map(|i| {
            let card = &st.slots[i].card.as_ref()?.ui;
            let SlintSource::Text(text) = &card.source else { return None };
            telescope_shell::source::heading(text, &card.component)
        }).collect()
    };
    titles.iter().filter_map(|t| ElementHandle::find_by_accessible_label(&r.w.ui, t).find(|e| e.accessible_checkable() == Some(true))).collect()
}

#[test]
fn folded_cards_draw_nothing_either() {
    let r = up("idle-redraws-folded");
    r.w.ui.window().set_size(slint::LogicalSize::new(1300.0, 9000.0));
    settle(&r);
    let buttons = fold_buttons(&r);
    assert!(buttons.len() >= 2, "{} cards fold", buttons.len());
    for fold in [true, false] {
        for button in &buttons {
            if button.accessible_checked() != Some(fold) {
                button.invoke_accessible_default_action();
            }
        }
        settle(&r);
        assert_eq!(redraws_over(&r, r.w.ui.window()), 0, "cards folded: {fold}");
    }
}

#[test]
fn a_stream_with_no_new_picture_draws_nothing() {
    let r = up("idle-redraws-stream");
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    settle(&r);
    assert!(r.w.is_streaming());
    assert_eq!(redraws_over(&r, r.w.ui.window()), 0);
}

#[test]
fn the_settings_window_draws_nothing_but_its_caret() {
    let r = up("idle-redraws-settings");
    telescope_shell::settings::open(&r.w, "");
    let settings = telescope_shell::settings::current().expect("the settings window");
    settings.ui.window().set_size(slint::LogicalSize::new(900.0, 700.0));
    for _ in 0..40 {
        r.skip(50);
        offscreen::redraw(settings.ui.window());
        offscreen::redraw(r.w.ui.window());
    }
    // The main window behind it stays still; the search box that has the focus blinks its caret, a few redraws a second at most
    assert_eq!(redraws_over(&r, r.w.ui.window()), 0);
    let blinks = redraws_over(&r, settings.ui.window());
    assert!(blinks <= 4 * SECONDS, "the settings window drew {blinks} times in {SECONDS} s");
    telescope_shell::settings::forget();
}

#[test]
fn banners_draw_nothing_once_they_have_opened_or_shut() {
    let r = up("idle-redraws-banners");
    r.show_issue("start", "Can't connect to the phone");
    r.show_issue("vcam", "Couldn't add more virtual cameras");
    settle(&r);
    assert_eq!(r.banners_up().len(), 2);
    assert_eq!(redraws_over(&r, r.w.ui.window()), 0, "two banners at rest");
    // One goes and its row with it, a message changes under the other: each ends at rest too
    r.w.state.borrow_mut().banners.clear(Some("start"), None);
    r.show_issue("vcam", "Still no more virtual cameras");
    settle(&r);
    assert_eq!(r.rows("banners").len(), 1, "the shut banner's row is gone");
    assert_eq!(redraws_over(&r, r.w.ui.window()), 0, "one banner at rest");
    r.w.state.borrow_mut().banners.clear(None, None);
    r.w.show_banners();
    settle(&r);
    assert!(r.rows("banners").is_empty());
    assert_eq!(redraws_over(&r, r.w.ui.window()), 0, "none");
}
