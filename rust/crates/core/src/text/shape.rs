//! A font that can both draw glyphs (ab_glyph) and shape text (harfrust): the same bytes serve both, so a glyph id from one
//! means the same glyph to the other.

use ab_glyph::{FontVec, VariableFont};
use harfrust::{BufferFlags, Direction, Feature, FontRef, Shaper, ShaperData, ShaperInstance, Tag, UnicodeBuffer, Variation};
use std::ops::{Deref, Range};
use std::sync::Arc;

struct Loaded {
    outlines: ab_glyph::FontArc,
    bytes: Box<[u8]>,
    shaper: ShaperData,
    instance: ShaperInstance,
}

/// A font ready to draw and to shape with. Clones share the same data.
#[derive(Clone)]
pub struct FontArc(Arc<Loaded>);

/// One glyph out of the shaper, in font units.
#[derive(Clone, Copy, Debug)]
pub struct Shaped {
    pub glyph: u16,
    /// Where its cluster starts in the text that was shaped (a byte offset).
    pub cluster: usize,
    pub x_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
}

impl FontArc {
    /// A font from its bytes (a collection gives its first font); a variable one is set to this weight, else its default look.
    pub fn from_bytes(bytes: Vec<u8>, weight: f32) -> Option<FontArc> {
        let mut outlines = FontVec::try_from_vec(bytes.clone()).ok()?;
        outlines.set_variation(b"wght", weight);
        let font = FontRef::from_index(&bytes, 0).ok()?;
        let shaper = ShaperData::new(&font);
        let instance = ShaperInstance::from_variations(&font, [Variation { tag: Tag::new(b"wght"), value: weight }]);
        Some(FontArc(Arc::new(Loaded { outlines: ab_glyph::FontArc::new(outlines), bytes: bytes.into_boxed_slice(), shaper, instance })))
    }

    /// Whether both are the same loaded font.
    pub fn same(&self, other: &FontArc) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Shapes `text[range]` as one run in one direction; clusters are offsets into `text`. Ligatures are off when `plain` is set
    /// (Qt does the same once letters are spaced apart).
    pub fn shape(&self, text: &str, range: Range<usize>, rtl: bool, plain: bool) -> Vec<Shaped> {
        let Ok(font) = FontRef::from_index(&self.0.bytes, 0) else { return Vec::new() };
        let shaper: Shaper = self.0.shaper.shaper(&font).instance(Some(&self.0.instance)).build();
        let mut buffer = UnicodeBuffer::new();
        for (i, ch) in text[range.clone()].char_indices() {
            buffer.add(ch, (range.start + i) as u32);
        }
        buffer.set_direction(if rtl { Direction::RightToLeft } else { Direction::LeftToRight });
        buffer.set_flags(BufferFlags::BEGINNING_OF_TEXT | BufferFlags::END_OF_TEXT);
        buffer.guess_segment_properties();
        let off = [Feature::new(Tag::new(b"liga"), 0, ..), Feature::new(Tag::new(b"clig"), 0, ..)];
        let features: &[Feature] = if plain { &off } else { &[] };
        let out = shaper.shape(buffer, harfrust::ShapeOptions::new().features(features));
        out.glyph_infos()
            .iter()
            .zip(out.glyph_positions())
            .map(|(info, pos)| Shaped { glyph: info.glyph_id as u16, cluster: info.cluster as usize, x_advance: pos.x_advance, x_offset: pos.x_offset, y_offset: pos.y_offset })
            .collect()
    }
}

/// The outlines, for everything ab_glyph does (glyph ids, metrics, drawing).
impl Deref for FontArc {
    type Target = ab_glyph::FontArc;
    fn deref(&self) -> &ab_glyph::FontArc {
        &self.0.outlines
    }
}
