//! Composite's auto director: picks which saved layout to show from which streams have hands in frame.
//!
//! The layout the user picked by hand is home, and the stream it shows biggest is the home stream. When hands stay in frame on another stream
//! (someone shows something to the desk camera) the layout that shows that stream biggest goes on. When the hands leave, home comes back. Every
//! switch waits for hands to stay (HOLD_IN, HOLD_OUT) and for a rest after the last switch (MIN_DWELL), so it never flickers between pictures.
//! This is a plain state machine over (time, stream, hands) so the rules are testable without a stream.

use crate::composite::Layout;
use std::collections::BTreeMap;

/// How long hands must stay in frame on another stream before its layout goes on.
pub const HOLD_IN: f64 = 3.0;
/// How long no other stream may show hands before home comes back.
pub const HOLD_OUT: f64 = 6.0;
/// The least time between two switches.
pub const MIN_DWELL: f64 = 5.0;
/// A reading older than this counts as nobody there.
pub const STALE_S: f64 = 2.0;

#[derive(Clone, Copy)]
struct Seen {
    /// When the stream last said.
    at: f64,
    /// Since when hands have been in frame without a gap, while they are.
    since: Option<f64>,
}

#[derive(Default)]
pub struct Director {
    seen: BTreeMap<String, Seen>,
    /// The saved layout that is home, "" for none.
    home: String,
    /// The stream whose layout is on instead of home.
    away: Option<String>,
    last_switch: f64,
    /// Since when no stream has shown hands, while it is away.
    quiet_since: Option<f64>,
}

/// The stream a layout shows biggest.
pub fn biggest(layout: &Layout) -> Option<&str> {
    layout.tiles.iter().max_by(|a, b| (a.w * a.h).total_cmp(&(b.w * b.h))).map(|t| t.source_id.as_str())
}

impl Director {
    pub fn new() -> Director {
        Director::default()
    }

    pub fn home(&self) -> &str {
        &self.home
    }

    /// The user picked a layout by hand: it is home now and the director rests a while.
    pub fn picked(&mut self, now: f64, name: &str) {
        self.home = name.to_string();
        self.away = None;
        self.quiet_since = None;
        self.last_switch = now;
    }

    /// Forgets what it was told, for when it goes off or the streams change.
    pub fn clear(&mut self) {
        *self = Director { home: std::mem::take(&mut self.home), ..Director::default() };
    }

    /// A stream said whether hands are in its frame.
    pub fn hear(&mut self, now: f64, stream: &str, hands: bool) {
        let entry = self.seen.entry(stream.to_string()).or_insert(Seen { at: now, since: None });
        let continuing = hands && entry.since.is_some() && now - entry.at <= STALE_S;
        entry.since = if hands { if continuing { entry.since } else { Some(now) } } else { None };
        entry.at = now;
    }

    fn hands(&self, now: f64, stream: &str) -> bool {
        self.seen.get(stream).is_some_and(|s| s.since.is_some() && now - s.at <= STALE_S)
    }

    fn held(&self, now: f64, stream: &str) -> bool {
        self.hands(now, stream) && self.seen.get(stream).and_then(|s| s.since).is_some_and(|since| now - since >= HOLD_IN)
    }

    /// The saved layout that shows stream biggest, other than home.
    fn layout_for<'a>(&self, layouts: &'a [Layout], stream: &str) -> Option<&'a str> {
        layouts.iter().find(|l| l.name != self.home && biggest(l) == Some(stream)).map(|l| l.name.as_str())
    }

    /// The name of the layout to switch to now, if it is time to.
    pub fn decide(&mut self, now: f64, layouts: &[Layout]) -> Option<String> {
        let home = layouts.iter().find(|l| l.name == self.home)?;
        let home_stream = biggest(home)?.to_string();
        if now - self.last_switch < MIN_DWELL {
            return None;
        }
        // the one that has had hands longest comes first
        let mut candidates: Vec<(&String, f64)> = self.seen.iter()
            .filter(|(s, _)| **s != home_stream && self.away.as_ref() != Some(*s) && self.held(now, s) && self.layout_for(layouts, s).is_some())
            .filter_map(|(s, seen)| seen.since.map(|since| (s, since)))
            .collect();
        candidates.sort_by(|a, b| a.1.total_cmp(&b.1));
        let next = candidates.first().map(|(s, _)| (*s).clone());
        match self.away.clone() {
            None => {
                let stream = next?;
                self.go_away(now, &stream, layouts)
            }
            Some(current) => {
                if self.hands(now, &current) {
                    self.quiet_since = None;
                    return None;
                }
                let quiet = *self.quiet_since.get_or_insert(now);
                if let Some(stream) = next {
                    return self.go_away(now, &stream, layouts);
                }
                if now - quiet >= HOLD_OUT {
                    self.away = None;
                    self.quiet_since = None;
                    self.last_switch = now;
                    return Some(self.home.clone());
                }
                None
            }
        }
    }

    fn go_away(&mut self, now: f64, stream: &str, layouts: &[Layout]) -> Option<String> {
        let name = self.layout_for(layouts, stream)?.to_string();
        self.away = Some(stream.to_string());
        self.quiet_since = None;
        self.last_switch = now;
        Some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composite::Tile;

    fn layout(name: &str, big: &str, small: &str) -> Layout {
        Layout::named(name, vec![Tile::boxed(big, "", 0.0, 0.0, 0.7, 1.0), Tile::boxed(small, "", 0.7, 0.0, 0.3, 0.4)])
    }

    fn saved() -> Vec<Layout> {
        vec![layout("Face", "face", "desk"), layout("Desk", "desk", "face"), layout("Wall", "wall", "face")]
    }

    /// Streams say what they see every half second from `from` to `to`.
    fn run(d: &mut Director, layouts: &[Layout], from: f64, to: f64, hands: &[(&str, bool)]) -> Vec<(f64, String)> {
        let mut out = Vec::new();
        let mut t = from;
        while t < to {
            for (s, h) in hands {
                d.hear(t, s, *h);
            }
            if let Some(name) = d.decide(t, layouts) {
                out.push((t, name));
            }
            t += 0.5;
        }
        out
    }

    fn started() -> Director {
        let mut d = Director::new();
        d.picked(0.0, "Face");
        d
    }

    #[test]
    fn hands_on_the_desk_for_a_while_bring_the_desk_layout_and_leaving_brings_home_back() {
        let (l, mut d) = (saved(), started());
        let a = run(&mut d, &l, 6.0, 16.0, &[("face", true), ("desk", true)]);
        assert_eq!(a.len(), 1, "one switch, not a flicker: {a:?}");
        assert_eq!(a[0].1, "Desk");
        assert!((a[0].0 - 9.0).abs() < 0.6, "after the hands stayed 3 s: {a:?}");
        let b = run(&mut d, &l, 16.0, 30.0, &[("face", true), ("desk", false)]);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].1, "Face");
        assert!(b[0].0 >= 16.0 + HOLD_OUT - 0.1, "home waits for 6 s without hands: {b:?}");
    }

    #[test]
    fn hands_that_come_and_go_quickly_switch_nothing() {
        let (l, mut d) = (saved(), started());
        let mut switches = 0;
        let mut t = 6.0;
        while t < 40.0 {
            // two seconds of hands, then a gap, over and over
            let hands = (t as u64 % 4) < 2;
            d.hear(t, "desk", hands);
            switches += d.decide(t, &l).is_some() as usize;
            t += 0.5;
        }
        assert_eq!(switches, 0);
    }

    #[test]
    fn a_short_gap_while_away_doesnt_send_it_home() {
        let (l, mut d) = (saved(), started());
        assert_eq!(run(&mut d, &l, 6.0, 12.0, &[("desk", true)]).len(), 1);
        assert!(run(&mut d, &l, 12.0, 15.0, &[("desk", false)]).is_empty(), "three quiet seconds is under HOLD_OUT");
        assert!(run(&mut d, &l, 15.0, 30.0, &[("desk", true)]).is_empty(), "hands are back, nothing to change");
    }

    #[test]
    fn two_away_streams_dont_ping_pong_and_the_current_one_is_kept_while_it_has_hands() {
        let (l, mut d) = (saved(), started());
        run(&mut d, &l, 6.0, 12.0, &[("desk", true)]);
        let both = run(&mut d, &l, 12.0, 40.0, &[("desk", true), ("wall", true)]);
        assert!(both.is_empty(), "the desk keeps it while its hands are there: {both:?}");
        let moved = run(&mut d, &l, 40.0, 50.0, &[("desk", false), ("wall", true)]);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].1, "Wall");
    }

    #[test]
    fn nothing_switches_within_the_rest_after_a_hand_pick_or_a_switch() {
        let (l, mut d) = (saved(), started());
        let early = run(&mut d, &l, 0.5, 4.5, &[("desk", true)]);
        assert!(early.is_empty(), "the pick at 0 s rests it until 5 s: {early:?}");
        let late = run(&mut d, &l, 4.5, 8.0, &[("desk", true)]);
        assert_eq!(late.len(), 1);
        d.picked(8.0, "Face");
        assert_eq!(d.home(), "Face");
        assert!(run(&mut d, &l, 8.5, 12.5, &[("desk", true)]).is_empty(), "rests after the pick too");
    }

    #[test]
    fn a_stale_reading_counts_as_nobody_and_the_home_stream_never_wins() {
        let (l, mut d) = (saved(), started());
        d.hear(6.0, "desk", true);
        assert_eq!(d.decide(9.5, &l), None, "the last reading is 3.5 s old: gone");
        let (l, mut d) = (saved(), started());
        assert!(run(&mut d, &l, 6.0, 30.0, &[("face", true)]).is_empty(), "hands on the home stream change nothing");
    }

    #[test]
    fn without_a_home_layout_or_a_layout_for_the_stream_it_does_nothing() {
        let l = saved();
        let mut d = Director::new();
        assert!(run(&mut d, &l, 6.0, 20.0, &[("desk", true)]).is_empty(), "no home picked yet");
        let mut d = started();
        assert!(run(&mut d, &l, 6.0, 20.0, &[("other", true)]).is_empty(), "no saved layout shows that stream big");
    }
}
