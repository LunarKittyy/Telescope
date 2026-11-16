//! The Card order dialog (ui/card-order.slint): which cards it lists, what a drop or an Alt+arrow does to the order, Reset, Revert and
//! Restart. The order is kept by `State` (shell.rs) and applies from the next start, so nothing here moves a card on screen.

use crate::layout::CardLayout;
use crate::shell::State;
use crate::window::Window;
use slint::{Model, ModelRc};
use slint_interpreter::{Struct, Value as SValue};
use std::rc::Rc;
use telescope_core::i18n::tr;
use telescope_core::ui::{SettingsSection, SlintSource, UiSpec};

/// What the settings window calls the owner of its Card order row, which the shell lists itself.
pub const SHELL: &str = "shell";

/// The row on the General tab that opens the dialog, after every plugin's sections.
pub fn settings_section() -> SettingsSection {
    SettingsSection {
        id: "card-order".into(),
        tab: "general".into(),
        ui: UiSpec { source: SlintSource::Text(include_str!("../ui/card-order-settings.slint").into()), component: "CardOrderSettings".into(), props: serde_json::json!({"card-group": "extras"}) },
        order: 1000,
        tab_title: String::new(),
        keywords: String::new(),
        footer: None,
    }
}

/// The two groups: the left rail, then the right rail.
const SIDES: usize = 2;

/// One card of the dialog: its plugin and where it is, `at` counting from the top of its rail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub name: String,
    pub side: usize,
    pub at: usize,
}

/// The cards of a layout, left rail first.
pub fn places(layout: &CardLayout) -> Vec<Place> {
    [&layout.left, &layout.right].into_iter().enumerate()
        .flat_map(|(side, names)| names.iter().enumerate().map(move |(at, name)| Place { name: name.clone(), side, at }))
        .collect()
}

/// The layout with `name` taken out and put at `at` among the other cards of `side` (past the end is the end). None when no card has the name
/// or it stays where it was.
pub fn moved(layout: &CardLayout, name: &str, side: usize, at: usize) -> Option<CardLayout> {
    if side >= SIDES {
        return None;
    }
    let mut rails = [layout.left.clone(), layout.right.clone()];
    let from = rails.iter().position(|r| r.iter().any(|n| n == name))?;
    let old = rails[from].iter().position(|n| n == name)?;
    let card = rails[from].remove(old);
    let at = at.min(rails[side].len());
    rails[side].insert(at, card);
    let [left, right] = rails;
    let next = CardLayout { left, right };
    (next != *layout).then_some(next)
}

/// `shown` (the cards that are loaded) with the names the settings file keeps for plugins that are off put back: each after the card that came
/// before it in its old rail, wherever that card is now, or at the top of its old rail when nothing before it is left. So a plugin turned back
/// on finds its place.
pub fn with_absent(shown: CardLayout, file: Option<&CardLayout>) -> CardLayout {
    let Some(file) = file else { return shown };
    let mut rails = [shown.left, shown.right];
    let present = |rails: &[Vec<String>; SIDES], name: &str| rails.iter().any(|r| r.iter().any(|n| n == name));
    let absent: Vec<(usize, Vec<String>)> = [&file.left, &file.right].into_iter().enumerate().map(|(side, names)| (side, names.clone())).collect();
    for (side, names) in absent {
        let mut anchor: Option<String> = None;
        let mut top = 0;
        for name in names {
            if present(&rails, &name) {
                anchor = Some(name);
                continue;
            }
            let spot = anchor.as_ref().and_then(|a| rails.iter().enumerate().find_map(|(s, r)| Some((s, r.iter().position(|n| n == a)? + 1))));
            let (to, at) = spot.unwrap_or_else(|| {
                top += 1;
                (side, top - 1)
            });
            rails[to].insert(at, name.clone());
            anchor = Some(name);
        }
    }
    let [left, right] = rails;
    CardLayout { left, right }
}

/// A drop or a key put `name` at `at` among the other cards of `side`: saved at once. False when nothing changed.
pub fn move_card(state: &mut State, name: &str, side: usize, at: usize) -> bool {
    let Some(next) = moved(&state.saved_card_layout(), name, side, at) else { return false };
    let all = with_absent(next, state.stored_card_layout());
    state.set_saved_card_layout(all);
    true
}

/// Reset order: the default order at the next start.
pub fn reset(state: &mut State) {
    state.clear_saved_card_layout();
}

/// Revert: the order the running app has again, so no restart is waiting. The default order is no saved order at all.
pub fn revert(state: &mut State) {
    let running = state.running_card_layout();
    if state.panels.layout_for(None) == running {
        state.clear_saved_card_layout();
    } else {
        let all = with_absent(running, state.stored_card_layout());
        state.set_saved_card_layout(all);
    }
}

impl Window {
    /// The dialog's callbacks and its rows' model; the right-click menu of a card header comes in through ui-kit's CardOrder global.
    pub(crate) fn wire_card_order(self: &Rc<Self>) {
        let ui = &self.ui;
        let _ = ui.set_property("order-rows", SValue::Model(ModelRc::from(self.order_rows.clone())));
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("order-moved", move |args| {
            if let (Some(w), [SValue::String(name), SValue::Number(side), SValue::Number(at)]) = (weak.upgrade(), args) {
                move_card(&mut w.state.borrow_mut(), name, *side as usize, *at as usize);
                w.card_order_changed();
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("order-reset", move |_| {
            if let Some(w) = weak.upgrade() {
                reset(&mut w.state.borrow_mut());
                w.card_order_changed();
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("order-revert", move |_| {
            if let Some(w) = weak.upgrade() {
                revert(&mut w.state.borrow_mut());
                w.card_order_changed();
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_callback("order-restart", move |_| {
            if let Some(w) = weak.upgrade() {
                w.restart_from_card_order();
            }
            SValue::Void
        });
        let weak = Rc::downgrade(self);
        let _ = ui.set_global_callback("CardOrder", "open", move |_| {
            if let Some(w) = weak.upgrade() {
                w.open_card_order();
            }
            SValue::Void
        });
    }

    /// Shows the dialog over the window.
    pub fn open_card_order(self: &Rc<Self>) {
        self.card_order_changed();
        let _ = self.ui.invoke("open-card-order", &[]);
    }

    /// Whatever the person did in the dialog: the rows and the restart bar as they are now, and any ask to restart is forgotten.
    fn card_order_changed(&self) {
        let _ = self.ui.set_property("order-confirming", SValue::Bool(false));
        self.show_card_order();
    }

    /// Restart now. While streaming the bar asks once more first, as the Plugins tab asks "Restart Telescope now?".
    fn restart_from_card_order(&self) {
        let confirming = matches!(self.ui.get_property("order-confirming"), Ok(SValue::Bool(true)));
        if self.is_streaming() && !confirming {
            let _ = self.ui.set_property("order-confirming", SValue::Bool(true));
            self.show_card_order();
        } else if !self.restart_app() {
            let _ = self.ui.set_property("order-note", SValue::String(crate::plugins_tab::restart_failed().text.into()));
        }
    }

    /// Puts the dialog's rows, counts and restart bar right. The rows keep the running order and change in place, so a row that moves keeps its element.
    fn show_card_order(&self) {
        let (running, next, pending, categories) = {
            let st = self.state.borrow();
            let categories: Vec<(String, String)> = st.slots.iter().map(|s| (s.info.name.clone(), s.info.category.clone())).collect();
            (st.running_card_layout(), st.saved_card_layout(), st.card_layout_restart_pending(), categories)
        };
        let places = places(&next);
        let own = |plugin: &str, prop: &str| self.pieces.iter().find(|r| r.plugin == plugin && r.ui == "card").map(|r| format!("{}-card-{prop}", r.id));
        let rows: Vec<SValue> = running.left.iter().chain(&running.right).filter_map(|name| {
            let place = places.iter().find(|p| p.name == *name)?;
            let title = match own(name, "title").map(|p| self.ui.get_property(&p)) {
                Some(Ok(SValue::String(t))) if !t.is_empty() => t.to_string(),
                _ => name.clone(),
            };
            let icon = match own(name, "icon").map(|p| self.ui.get_property(&p)) {
                Some(Ok(SValue::Image(i))) => i,
                _ => slint::Image::default(),
            };
            let category = categories.iter().find(|(n, _)| n == name).map(|(_, c)| c.clone()).unwrap_or_default();
            Some(SValue::Struct(Struct::from_iter([
                ("name".to_string(), SValue::String(name.as_str().into())),
                ("title".to_string(), SValue::String(title.into())),
                ("icon".to_string(), SValue::Image(icon)),
                ("category".to_string(), SValue::String(category.into())),
                ("side".to_string(), SValue::Number(place.side as f64)),
                ("at".to_string(), SValue::Number(place.at as f64)),
            ])))
        }).collect();
        let count = rows.len();
        for (i, row) in rows.into_iter().enumerate() {
            match self.order_rows.row_data(i) {
                Some(old) if crate::same::equal(&old, &row) => {}
                Some(_) => self.order_rows.set_row_data(i, row),
                None => self.order_rows.push(row),
            }
        }
        while self.order_rows.row_count() > count {
            self.order_rows.remove(self.order_rows.row_count() - 1);
        }
        let confirming = matches!(self.ui.get_property("order-confirming"), Ok(SValue::Bool(true))) && pending;
        let note = if confirming { tr("Streaming stops while Telescope restarts. Restarting takes a few seconds.") } else { tr("Restart to apply changes.") };
        let ui = &self.ui;
        let _ = crate::same::set(ui, "order-left-count", SValue::Number(next.left.len() as f64));
        let _ = crate::same::set(ui, "order-right-count", SValue::Number(next.right.len() as f64));
        let _ = crate::same::set(ui, "order-confirming", SValue::Bool(confirming));
        let _ = crate::same::set(ui, "order-note", SValue::String(note.into()));
        let _ = crate::same::set(ui, "order-pending", SValue::Bool(pending));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(left: &[&str], right: &[&str]) -> CardLayout {
        let own = |names: &[&str]| names.iter().map(|n| n.to_string()).collect();
        CardLayout { left: own(left), right: own(right) }
    }

    #[test]
    fn places_count_from_the_top_of_each_rail() {
        let all = places(&layout(&["a", "b"], &["c"]));
        assert_eq!(all, [
            Place { name: "a".into(), side: 0, at: 0 },
            Place { name: "b".into(), side: 0, at: 1 },
            Place { name: "c".into(), side: 1, at: 0 },
        ]);
    }

    #[test]
    fn a_card_moves_down_its_rail() {
        // at counts the other cards: after b and c, c ends the list
        assert_eq!(moved(&layout(&["a", "b", "c"], &[]), "a", 0, 2), Some(layout(&["b", "c", "a"], &[])));
        assert_eq!(moved(&layout(&["a", "b", "c"], &[]), "a", 0, 1), Some(layout(&["b", "a", "c"], &[])));
    }

    #[test]
    fn a_card_moves_up_its_rail() {
        assert_eq!(moved(&layout(&["a", "b", "c"], &[]), "c", 0, 0), Some(layout(&["c", "a", "b"], &[])));
        assert_eq!(moved(&layout(&["a", "b", "c"], &[]), "c", 0, 1), Some(layout(&["a", "c", "b"], &[])));
    }

    #[test]
    fn a_card_moves_into_the_other_rail() {
        assert_eq!(moved(&layout(&["a", "b"], &["c", "d"]), "a", 1, 1), Some(layout(&["b"], &["c", "a", "d"])));
        assert_eq!(moved(&layout(&["a", "b"], &["c", "d"]), "d", 0, 0), Some(layout(&["d", "a", "b"], &["c"])));
    }

    #[test]
    fn an_empty_rail_takes_a_drop_and_one_can_be_emptied() {
        assert_eq!(moved(&layout(&["a"], &[]), "a", 1, 0), Some(layout(&[], &["a"])));
        assert_eq!(moved(&layout(&["a", "b"], &[]), "b", 1, 5), Some(layout(&["a"], &["b"])));
    }

    #[test]
    fn a_drop_where_it_was_changes_nothing() {
        assert_eq!(moved(&layout(&["a", "b"], &["c"]), "b", 0, 1), None);
        assert_eq!(moved(&layout(&["a", "b"], &["c"]), "c", 1, 0), None);
        assert_eq!(moved(&layout(&["a", "b"], &["c"]), "a", 0, 0), None);
    }

    #[test]
    fn an_unknown_card_or_rail_changes_nothing() {
        assert_eq!(moved(&layout(&["a"], &[]), "ghost", 0, 0), None);
        assert_eq!(moved(&layout(&["a"], &[]), "a", 2, 0), None);
    }

    #[test]
    fn names_of_plugins_that_are_off_come_back_after_the_card_before_them() {
        let file = layout(&["a", "off", "b"], &["c", "gone"]);
        let all = with_absent(layout(&["b", "a"], &["c"]), Some(&file));
        // off followed a, which is now second on the left; gone followed c
        assert_eq!(all, layout(&["b", "a", "off"], &["c", "gone"]));
    }

    #[test]
    fn an_absent_name_follows_its_card_into_the_other_rail() {
        let file = layout(&["a", "off"], &["c"]);
        assert_eq!(with_absent(layout(&["c"], &["a"]), Some(&file)), layout(&["c"], &["a", "off"]));
    }

    #[test]
    fn an_absent_name_with_nothing_before_it_goes_to_the_top_of_its_rail_and_several_keep_their_order() {
        let file = layout(&["x", "y", "a"], &["z"]);
        assert_eq!(with_absent(layout(&["a", "b"], &["c"]), Some(&file)), layout(&["x", "y", "a", "b"], &["z", "c"]));
    }

    #[test]
    fn without_a_file_order_nothing_is_added() {
        assert_eq!(with_absent(layout(&["a"], &["b"]), None), layout(&["a"], &["b"]));
    }
}
