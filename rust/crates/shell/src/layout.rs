//! Where the cards go as the window changes width.

pub use telescope_core::config::CardLayout;
use telescope_core::plugin::PLUGIN_CATEGORIES;

/// The window's mode changes here (the generated document reads these).
pub const WIDTH_THREE_COL: f32 = 1300.0;
pub const WIDTH_TWO_COL: f32 = 900.0;
/// Both rails share one width so the preview sits on the window's centre line.
pub const RAIL_WIDTH: f32 = 412.0;
/// Two-column mode: the one rail holding every card.
pub const RAIL_WIDTH_SOLO: f32 = 440.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Three,
    Two,
    One,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Left,
    Center,
    Right,
}

impl Region {
    /// A region a plugin names; anything unknown goes left, as in register_plugin.
    pub fn from_name(name: &str) -> Region {
        match name {
            "center" => Region::Center,
            "right" => Region::Right,
            _ => Region::Left,
        }
    }
}

/// The order key _place_panel sorts a side column by: pinned cards first, then by group.
pub fn order_key(pinned: bool, category: &str) -> (u8, usize) {
    let groups = PLUGIN_CATEGORIES.len();
    let group = PLUGIN_CATEGORIES.iter().position(|(id, _)| *id == category).unwrap_or(groups);
    if pinned { (0, 0) } else { (1, group) }
}

/// One side card as it was registered: where its plugin puts it by default, and what to sort it by.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    card: usize,
    name: String,
    region: Region,
    key: (u8, usize),
}

/// The cards of each region in the order they show, by index into whatever list the caller keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panels {
    pub left: Vec<usize>,
    pub center: Vec<usize>,
    pub right: Vec<usize>,
    entries: Vec<Entry>,
}

impl Panels {
    /// Pinned cards at the top of a side column, then the others by group, each group in the order registered; the
    /// stage's column keeps the order registered.
    pub fn place(&mut self, region: Region, card: usize, pinned: bool, category: &str) {
        self.place_named(region, card, pinned, category, "");
    }

    /// As place, for a card whose plugin has a name a saved layout can use.
    pub fn place_named(&mut self, region: Region, card: usize, pinned: bool, category: &str, name: &str) {
        if region == Region::Center {
            self.center.push(card);
            return;
        }
        self.entries.push(Entry { card, name: name.to_string(), region, key: order_key(pinned, category) });
        let (left, right) = self.arranged(None);
        (self.left, self.right) = (left, right);
    }

    pub fn remove(&mut self, card: usize) {
        for list in [&mut self.left, &mut self.center, &mut self.right] {
            list.retain(|&c| c != card);
        }
        self.entries.retain(|e| e.card != card);
    }

    /// Follow a saved layout from now on (call it once the cards are placed). Cards it doesn't list keep their default place.
    pub fn apply_layout(&mut self, layout: &CardLayout) {
        let (left, right) = self.arranged(Some(layout));
        (self.left, self.right) = (left, right);
    }

    /// The two rails as they show now, by plugin name (cards without a name are left out).
    pub fn layout(&self) -> CardLayout {
        self.named(&self.left, &self.right)
    }

    /// The rails a saved layout (None: the default order) would give these cards, by plugin name.
    pub fn layout_for(&self, saved: Option<&CardLayout>) -> CardLayout {
        let (left, right) = self.arranged(saved);
        self.named(&left, &right)
    }

    fn named(&self, left: &[usize], right: &[usize]) -> CardLayout {
        let names = |cards: &[usize]| {
            cards.iter().filter_map(|c| self.entries.iter().find(|e| e.card == *c)).filter(|e| !e.name.is_empty()).map(|e| e.name.clone()).collect()
        };
        CardLayout { left: names(left), right: names(right) }
    }

    /// The side rails: the default order, or the saved layout with each card it lacks put after its default neighbour.
    fn arranged(&self, saved: Option<&CardLayout>) -> (Vec<usize>, Vec<usize>) {
        let rail = |region: Region| {
            let mut cards: Vec<&Entry> = self.entries.iter().filter(|e| e.region == region).collect();
            cards.sort_by_key(|e| e.key);
            cards.into_iter().map(|e| e.card).collect::<Vec<_>>()
        };
        let (default_left, default_right) = (rail(Region::Left), rail(Region::Right));
        let Some(saved) = saved else { return (default_left, default_right) };
        let mut rails = [Vec::new(), Vec::new()];
        let mut placed = std::collections::BTreeSet::new();
        for (side, names) in [&saved.left, &saved.right].into_iter().enumerate() {
            for name in names.iter().filter(|n| !n.is_empty()) {
                if let Some(e) = self.entries.iter().find(|e| e.name == *name) {
                    if placed.insert(e.card) {
                        rails[side].push(e.card);
                    }
                }
            }
        }
        let listed = placed.clone();
        let default_order: Vec<usize> = default_left.iter().chain(&default_right).copied().collect();
        for (i, &card) in default_order.iter().enumerate() {
            if listed.contains(&card) {
                continue;
            }
            let after = default_order[..i].iter().rev().find(|c| placed.contains(c));
            let spot = after.and_then(|a| rails.iter().enumerate().find_map(|(side, r)| Some((side, r.iter().position(|c| c == a)? + 1))));
            let (side, at) = spot.unwrap_or_else(|| {
                let side = usize::from(default_right.contains(&card));
                (side, rails[side].len())
            });
            rails[side].insert(at, card);
            placed.insert(card);
        }
        let [left, right] = rails;
        (left, right)
    }

    /// The three columns for a mode: their cards and fixed width (None: it takes the rest).
    pub fn columns(&self, mode: Mode) -> [(Vec<usize>, Option<f32>); 3] {
        let cat = |a: &[usize], b: &[usize]| a.iter().chain(b).copied().collect::<Vec<_>>();
        match mode {
            Mode::Three => [
                (self.left.clone(), Some(RAIL_WIDTH)),
                (self.center.clone(), None),
                (self.right.clone(), Some(RAIL_WIDTH)),
            ],
            Mode::Two => [(cat(&self.left, &self.right), Some(RAIL_WIDTH_SOLO)), (self.center.clone(), None), (vec![], None)],
            Mode::One => [(cat(&cat(&self.center, &self.left), &self.right), None), (vec![], None), (vec![], None)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_first_then_groups_in_registration_order() {
        let mut p = Panels::default();
        p.place(Region::Left, 0, false, "look");
        p.place(Region::Left, 1, false, "camera");
        p.place(Region::Left, 2, true, "streaming");
        p.place(Region::Left, 3, false, "nonsense");
        p.place(Region::Left, 4, false, "camera");
        assert_eq!(p.left, vec![2, 1, 4, 0, 3]);
    }

    #[test]
    fn side_columns_put_the_pinned_cards_first_then_sort_by_group() {
        let cards = [("fun", "look", false), ("photos", "camera", false), ("tag", "on_screen", false), ("camera", "camera", true), ("grade", "look", false), ("odd", "nowhere", false)];
        let mut p = Panels::default();
        for (i, (_, category, pinned)) in cards.iter().enumerate() {
            p.place(Region::Right, i, *pinned, category);
        }
        let keys: Vec<(u8, usize)> = p.right.iter().map(|&i| order_key(cards[i].2, cards[i].1)).collect();
        assert_eq!(keys, [(0, 0), (1, 0), (1, 1), (1, 1), (1, 4), (1, 8)]);
    }

    #[test]
    fn register_plugin_routes_unknown_region_to_the_left_rail() {
        let mut p = Panels::default();
        p.place(Region::from_name("nowhere"), 0, false, "camera");
        assert_eq!(p.columns(Mode::Three)[0].0, [0]);
    }

    // Cards 0..5 named a..e, default left [0, 1, 2], right [3, 4]
    fn named() -> Panels {
        let mut p = Panels::default();
        for (card, (name, region)) in [("a", Region::Left), ("b", Region::Left), ("c", Region::Left), ("d", Region::Right), ("e", Region::Right)].into_iter().enumerate() {
            p.place_named(region, card, false, "camera", name);
        }
        p
    }

    fn saved(left: &[&str], right: &[&str]) -> CardLayout {
        let own = |names: &[&str]| names.iter().map(|n| n.to_string()).collect();
        CardLayout { left: own(left), right: own(right) }
    }

    #[test]
    fn no_saved_layout_is_the_default_order() {
        let p = named();
        assert_eq!((p.left.clone(), p.right.clone()), (vec![0, 1, 2], vec![3, 4]));
        assert_eq!(p.layout(), saved(&["a", "b", "c"], &["d", "e"]));
        assert_eq!(p.layout_for(None), p.layout());
    }

    #[test]
    fn a_saved_layout_is_followed_and_can_move_a_card_to_the_other_rail() {
        let mut p = named();
        p.apply_layout(&saved(&["c", "d", "a"], &["e", "b"]));
        assert_eq!((p.left.clone(), p.right.clone()), (vec![2, 3, 0], vec![4, 1]));
        assert_eq!(p.layout(), saved(&["c", "d", "a"], &["e", "b"]));
    }

    #[test]
    fn the_preview_is_never_moved() {
        let mut p = named();
        p.place_named(Region::Center, 5, false, "preview", "preview");
        p.apply_layout(&saved(&["preview", "a"], &["b"]));
        assert_eq!(p.center, [5]);
        assert!(!p.left.contains(&5) && !p.right.contains(&5));
    }

    #[test]
    fn names_of_plugins_that_are_not_loaded_are_ignored() {
        let mut p = named();
        p.apply_layout(&saved(&["ghost", "b", "a", "c"], &["e", "gone", "d"]));
        assert_eq!((p.left.clone(), p.right.clone()), (vec![1, 0, 2], vec![4, 3]));
        assert_eq!(p.layout(), saved(&["b", "a", "c"], &["e", "d"]));
    }

    #[test]
    fn a_card_the_layout_lacks_goes_after_its_default_neighbour() {
        let mut p = named();
        p.apply_layout(&saved(&["c", "a"], &["d", "e"]));
        // b follows a (the card before it in the default order), wherever a is
        assert_eq!((p.left.clone(), p.right.clone()), (vec![2, 0, 1], vec![3, 4]));
    }

    #[test]
    fn a_new_card_follows_its_neighbour_into_the_other_rail() {
        let mut p = named();
        p.apply_layout(&saved(&["c"], &["d", "a", "e"]));
        assert_eq!((p.left.clone(), p.right.clone()), (vec![2], vec![3, 0, 1, 4]));
    }

    #[test]
    fn new_cards_in_a_row_keep_their_default_order() {
        let mut p = named();
        p.apply_layout(&saved(&["a"], &["e"]));
        assert_eq!((p.left.clone(), p.right.clone()), (vec![0, 1, 2, 3], vec![4]));
    }

    #[test]
    fn a_new_card_with_no_known_neighbour_goes_to_the_bottom_of_its_own_rail() {
        let mut p = named();
        p.apply_layout(&saved(&["e"], &["b"]));
        // a has nothing before it: bottom of the left rail. c follows b (right rail), d follows c
        assert_eq!((p.left.clone(), p.right.clone()), (vec![4, 0], vec![1, 2, 3]));
        let mut q = named();
        q.apply_layout(&saved(&[], &["e", "d"]));
        assert_eq!((q.left.clone(), q.right.clone()), (vec![0, 1, 2], vec![4, 3]));
    }

    #[test]
    fn layout_for_says_what_a_saved_layout_would_give_without_changing_anything() {
        let p = named();
        assert_eq!(p.layout_for(Some(&saved(&["b"], &["a"]))), saved(&["b", "c", "d", "e"], &["a"]));
        assert_eq!(p.layout(), saved(&["a", "b", "c"], &["d", "e"]));
    }

    #[test]
    fn modes_merge_columns() {
        let mut p = Panels::default();
        p.place(Region::Left, 0, false, "camera");
        p.place(Region::Center, 1, false, "preview");
        p.place(Region::Right, 2, false, "look");
        assert_eq!(p.columns(Mode::Two)[0], (vec![0, 2], Some(RAIL_WIDTH_SOLO)));
        assert_eq!(p.columns(Mode::Two)[2], (vec![], None));
        assert_eq!(p.columns(Mode::One)[0], (vec![1, 0, 2], None));
        assert_eq!(p.columns(Mode::One)[1], (vec![], None));
        assert_eq!(p.columns(Mode::One)[2], (vec![], None));
        p.remove(1);
        assert!(p.center.is_empty());
    }
}
