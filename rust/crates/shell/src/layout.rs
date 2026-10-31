//! Where the cards go as the window changes width (app.py _refresh_layout and _place_panel).

use telescope_core::plugin::PLUGIN_CATEGORIES;

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

pub fn mode_for(width: f32) -> Mode {
    if width >= WIDTH_THREE_COL {
        Mode::Three
    } else if width >= WIDTH_TWO_COL {
        Mode::Two
    } else {
        Mode::One
    }
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

/// The cards of each region in the order they show, by index into whatever list the caller keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Panels {
    pub left: Vec<usize>,
    pub center: Vec<usize>,
    pub right: Vec<usize>,
    keys: Vec<(usize, (u8, usize))>,
}

impl Panels {
    /// Pinned cards at the top of a side column, then the others by group, each group in the order registered; the
    /// stage's column keeps the order registered.
    pub fn place(&mut self, region: Region, card: usize, pinned: bool, category: &str) {
        let list = match region {
            Region::Center => {
                self.center.push(card);
                return;
            }
            Region::Left => &mut self.left,
            Region::Right => &mut self.right,
        };
        let key = order_key(pinned, category);
        self.keys.push((card, key));
        let last = (1, PLUGIN_CATEGORIES.len());
        let keys = &self.keys;
        let key_of = |c: usize| keys.iter().find(|(k, _)| *k == c).map_or(last, |(_, v)| *v);
        let at = list.iter().position(|&c| key_of(c) > key).unwrap_or(list.len());
        list.insert(at, card);
    }

    pub fn remove(&mut self, card: usize) {
        for list in [&mut self.left, &mut self.center, &mut self.right] {
            list.retain(|&c| c != card);
        }
        self.keys.retain(|(c, _)| *c != card);
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
    fn widths_pick_the_mode() {
        assert_eq!(mode_for(1300.0), Mode::Three);
        assert_eq!(mode_for(1299.0), Mode::Two);
        assert_eq!(mode_for(900.0), Mode::Two);
        assert_eq!(mode_for(560.0), Mode::One);
    }

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

    #[test]
    fn modes_merge_columns() {
        let mut p = Panels::default();
        p.place(Region::Left, 0, false, "camera");
        p.place(Region::Center, 1, false, "preview");
        p.place(Region::Right, 2, false, "look");
        assert_eq!(p.columns(Mode::Two)[0], (vec![0, 2], Some(RAIL_WIDTH_SOLO)));
        assert_eq!(p.columns(Mode::One)[0], (vec![1, 0, 2], None));
        p.remove(1);
        assert!(p.center.is_empty());
    }
}
