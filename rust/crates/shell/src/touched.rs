//! The controls an outside caller (the MCP server) just touched, as the window shows them: an outline that stays a moment and fades.
//! This is the list behind the kit's `Touched` global. The controls carry their setting's id and draw the outline themselves, so a
//! control that isn't on screen (a folded card, another tab) simply isn't drawn and nothing is opened or scrolled to. Each setting has its own
//! expiry, so touching another control doesn't keep this one lit.

use std::time::{Duration, Instant};

/// How long an outline holds before it starts to fade, and how long the fade takes (the kit's TouchRing animates 500 ms).
pub const HOLD: Duration = Duration::from_millis(1000);
pub const FADE: Duration = Duration::from_millis(600);
/// More than this at once is a sweep over every setting, which would outline the whole card: only the first few show.
pub const MOST: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub on: bool,
    pub changed: bool,
    /// When it was last touched, and when it began to fade.
    at: Instant,
    faded_at: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct Touches {
    items: Vec<Item>,
}

impl Touches {
    /// These settings were just touched: outlined from now (again, if they were fading). A read doesn't weaken a change that is still showing.
    pub fn touch(&mut self, ids: &[String], changed: bool, now: Instant) {
        for id in ids.iter().filter(|id| !id.is_empty()).take(MOST) {
            match self.items.iter_mut().find(|i| i.id == *id) {
                Some(i) => {
                    i.changed = changed || (i.on && i.changed);
                    i.on = true;
                    i.at = now;
                    i.faded_at = None;
                }
                None => self.items.push(Item { id: id.clone(), on: true, changed, at: now, faded_at: None }),
            }
        }
    }

    /// Fades the outlines that have been held long enough and lets go of the ones that have faded, each by its own touch. Returns how long
    /// until the next one is due, or None when nothing is left to time.
    pub fn tick(&mut self, now: Instant) -> Option<Duration> {
        for i in &mut self.items {
            if i.on && now.saturating_duration_since(i.at) >= HOLD {
                i.on = false;
                i.faded_at = Some(now);
            }
        }
        self.items.retain(|i| i.on || i.faded_at.is_some_and(|f| now.saturating_duration_since(f) < FADE));
        self.items.iter().map(|i| match i.faded_at {
            None => HOLD.saturating_sub(now.saturating_duration_since(i.at)),
            Some(f) => FADE.saturating_sub(now.saturating_duration_since(f)),
        }).min()
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn shown(t: &Touches) -> Vec<(String, bool, bool)> {
        t.items().iter().map(|i| (i.id.clone(), i.on, i.changed)).collect()
    }

    #[test]
    fn touched_settings_show_then_fade_then_go() {
        let (mut t, start) = (Touches::default(), Instant::now());
        t.touch(&ids(&["colour_grading.contrast", "colour_grading.tint"]), true, start);
        assert!(t.items().iter().all(|i| i.on && i.changed));
        assert_eq!(t.tick(start + ms(500)), Some(HOLD - ms(500)));
        assert_eq!(t.tick(start + HOLD), Some(FADE));
        assert!(t.items().iter().all(|i| !i.on), "fading keeps them in the list so the outline can animate out");
        assert_eq!(t.tick(start + HOLD + FADE), None);
        assert!(t.is_empty());
    }

    #[test]
    fn a_touch_while_fading_brings_the_outline_back_and_a_read_does_not_weaken_a_change() {
        let (mut t, start) = (Touches::default(), Instant::now());
        t.touch(&ids(&["a.b"]), true, start);
        t.touch(&ids(&["a.b"]), false, start);
        assert_eq!(shown(&t), [("a.b".to_string(), true, true)]);
        t.tick(start + HOLD);
        let later = start + HOLD + ms(100);
        t.touch(&ids(&["a.b"]), false, later);
        assert_eq!(shown(&t), [("a.b".to_string(), true, false)], "a faded change comes back as the read it is now");
        t.tick(later + HOLD);
        t.touch(&ids(&["c.d"]), false, later + HOLD);
        t.tick(later + HOLD + FADE);
        assert_eq!(shown(&t), [("c.d".to_string(), true, false)]);
    }

    #[test]
    fn touching_another_setting_does_not_keep_an_old_outline_alive() {
        let (mut t, start) = (Touches::default(), Instant::now());
        t.touch(&ids(&["colour_grading.contrast"]), true, start);
        // The tint is read every half second, as an AI app watching it would
        for step in 1..=6 {
            t.touch(&ids(&["colour_grading.tint"]), false, start + ms(500 * step));
            t.tick(start + ms(500 * step));
        }
        let contrast = t.items().iter().find(|i| i.id == "colour_grading.contrast");
        assert!(contrast.is_none(), "contrast went after its own hold and fade although the tint kept being touched: {:?}", t.items());
        assert!(t.items().iter().any(|i| i.id == "colour_grading.tint" && i.on), "the tint, touched just now, is still lit");
    }

    #[test]
    fn a_sweep_over_every_setting_outlines_only_the_first_few() {
        let mut t = Touches::default();
        let many: Vec<String> = (0..100).map(|n| format!("p.k{n}")).collect();
        t.touch(&many, false, Instant::now());
        assert_eq!(t.items().len(), MOST);
    }
}
