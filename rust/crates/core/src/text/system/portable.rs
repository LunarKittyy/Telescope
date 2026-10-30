//! The system's font for a script where fontconfig is not the system's way: DirectWrite's own fallback on Windows (what Qt asks too) and
//! Core Text on a Mac, both through fontique.

use crate::text::FontArc;
use fontique::{Attributes, Blob, Collection, CollectionOptions, FallbackKey, FontStyle, FontWeight, FontWidth, QueryFamily, QueryStatus, Script, SourceCache};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use unicode_script::UnicodeScript;

/// One font the system offers for a script: loaded the first time a character it covers is asked for.
struct Candidate {
    blob: Blob<u8>,
    index: u32,
    weight: f32,
    covers: fontique::CharmapIndex,
    loaded: OnceLock<Option<FontArc>>,
}

impl Candidate {
    fn has(&self, ch: char) -> bool {
        self.covers.charmap(self.blob.as_ref()).is_some_and(|map| map.map(ch as u32).is_some_and(|glyph| glyph != 0))
    }

    fn font(&self) -> Option<&FontArc> {
        self.loaded.get_or_init(|| FontArc::from_bytes_at(self.blob.as_ref().to_vec(), self.index, self.weight)).as_ref()
    }
}

struct System {
    collection: Collection,
    sources: SourceCache,
    /// By script and the weight asked for (its bits).
    scripts: HashMap<(Script, u32), Vec<Candidate>>,
}

fn system() -> &'static Mutex<System> {
    static SYSTEM: OnceLock<Mutex<System>> = OnceLock::new();
    SYSTEM.get_or_init(|| {
        Mutex::new(System {
            collection: Collection::new(CollectionOptions { shared: false, system_fonts: true }),
            sources: SourceCache::default(),
            scripts: HashMap::new(),
        })
    })
}

/// The system's own font for this character in this weight, as Qt would pick it when the app's fonts lack it.
pub(in crate::text) fn font_for(ch: char, weight: f32) -> Option<FontArc> {
    let script = Script::from_bytes(ch.script().short_name().as_bytes().try_into().ok()?);
    let mut guard = system().lock().unwrap_or_else(|e| e.into_inner());
    let System { collection, sources, scripts } = &mut *guard;
    let candidates = scripts.entry((script, weight.to_bits())).or_insert_with(|| {
        let mut found = Vec::new();
        let mut query = collection.query(sources);
        query.set_families(std::iter::empty::<QueryFamily>());
        query.set_attributes(Attributes::new(FontWidth::NORMAL, FontStyle::Normal, FontWeight::new(weight)));
        query.set_fallbacks(FallbackKey::new(script, None));
        query.matches_with(|font| {
            let weight = font.synthesis.variation_settings().iter().find(|(tag, _)| tag.to_be_bytes() == *b"wght").map_or(weight, |&(_, w)| w);
            found.push(Candidate { blob: font.blob.clone(), index: font.index, weight, covers: font.charmap_index, loaded: OnceLock::new() });
            QueryStatus::Continue
        });
        found
    });
    candidates.iter().find(|c| c.has(ch)).and_then(|c| c.font().cloned())
}
