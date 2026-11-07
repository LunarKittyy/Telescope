//! The controls an outside caller (the MCP server) just touched, as the window shows them: an outline that stays a moment and fades.
//! This is the list behind the kit's `Touched` global. The controls carry their setting's id and draw the outline themselves, so a
//! control that isn't on screen (a folded card, another tab) simply isn't drawn and nothing is opened or scrolled to.

use std::time::Duration;

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
}

#[derive(Debug, Default)]
pub struct Touches {
    items: Vec<Item>,
}

impl Touches {
    /// These settings were just touched: outlined from now (again, if they were fading). A read doesn't weaken a change that is still showing.
    pub fn touch(&mut self, ids: &[String], changed: bool) {
        for id in ids.iter().filter(|id| !id.is_empty()).take(MOST) {
            match self.items.iter_mut().find(|i| i.id == *id) {
                Some(i) => {
                    i.changed = changed || (i.on && i.changed);
                    i.on = true;
                }
                None => self.items.push(Item { id: id.clone(), on: true, changed }),
            }
        }
    }

    /// Every outline starts to fade.
    pub fn fade(&mut self) {
        for i in &mut self.items {
            i.on = false;
        }
    }

    /// The faded ones go.
    pub fn clear_faded(&mut self) {
        self.items.retain(|i| i.on);
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

    #[test]
    fn touched_settings_show_then_fade_then_go() {
        let mut t = Touches::default();
        t.touch(&ids(&["colour_grading.contrast", "colour_grading.tint"]), true);
        assert!(t.items().iter().all(|i| i.on && i.changed));
        t.fade();
        assert!(t.items().iter().all(|i| !i.on), "fading keeps them in the list so the outline can animate out");
        t.clear_faded();
        assert!(t.is_empty());
    }

    #[test]
    fn a_touch_while_fading_brings_the_outline_back_and_a_read_does_not_weaken_a_change() {
        let mut t = Touches::default();
        t.touch(&ids(&["a.b"]), true);
        t.touch(&ids(&["a.b"]), false);
        assert_eq!(t.items(), [Item { id: "a.b".into(), on: true, changed: true }]);
        t.fade();
        t.touch(&ids(&["a.b"]), false);
        assert_eq!(t.items(), [Item { id: "a.b".into(), on: true, changed: false }], "a faded change comes back as the read it is now");
        t.fade();
        t.touch(&ids(&["c.d"]), false);
        t.clear_faded();
        assert_eq!(t.items().len(), 1);
        assert_eq!(t.items()[0].id, "c.d");
    }

    #[test]
    fn a_sweep_over_every_setting_outlines_only_the_first_few() {
        let mut t = Touches::default();
        let many: Vec<String> = (0..100).map(|n| format!("p.k{n}")).collect();
        t.touch(&many, false);
        assert_eq!(t.items().len(), MOST);
    }
}
