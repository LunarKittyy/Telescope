//! .cube LUT files (the second half of telescope/colour.py): reading Adobe's format plus Resolve's LUT_1D_INPUT_RANGE and
//! shaper-then-cube files, and writing a 3D LUT for other apps.

use std::path::Path;
use telescope_core::colour::sample_cube;
use telescope_core::i18n::{n_, tr};

/// The most points a side a 3D LUT file may have.
pub const MAX_CUBE_SIZE: usize = 65;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub const TOO_BIG: &str = n_("This file is too big to be a LUT Telescope can use.");
pub const NO_SIZE: &str = n_("This isn't a .cube LUT: it doesn't say its size.");
pub const BAD_LINE: &str = n_("Line {line} of the file can't be read.");
pub const NOT_A_COLOUR: &str = n_("Line {line} of the file isn't a colour (three numbers).");
pub const TOO_MANY_POINTS: &str = n_("This LUT has {size} points per side. Telescope takes up to {most}.");
pub const BAD_1D_SIZE: &str = n_("This LUT's 1D table has {size} entries, which isn't a size Telescope can use.");
pub const WRONG_COUNT: &str = n_("This LUT should have {expected} colours but has {found}.");
pub const NOT_NUMBERS: &str = n_("This LUT has colours that aren't numbers.");
pub const BAD_DOMAIN: &str = n_("This LUT's DOMAIN_MAX isn't above its DOMAIN_MIN.");

/// A .cube file Telescope can't use: the text (in English, untranslated) and the fields to fill into it once translated.
#[derive(Clone, Debug, PartialEq)]
pub struct CubeError {
    pub text: &'static str,
    pub fields: Vec<(&'static str, String)>,
}

impl CubeError {
    fn new(text: &'static str, fields: &[(&'static str, String)]) -> CubeError {
        CubeError { text, fields: fields.to_vec() }
    }

    fn fill(text: &str, fields: &[(&'static str, String)]) -> String {
        fields.iter().fold(text.to_string(), |t, (k, v)| t.replace(&format!("{{{k}}}"), v))
    }

    /// The problem in English, as Python's str(error).
    pub fn english(&self) -> String {
        CubeError::fill(self.text, &self.fields)
    }

    /// The problem in the language loaded.
    pub fn translated(&self) -> String {
        CubeError::fill(&tr(self.text), &self.fields)
    }
}

impl std::fmt::Display for CubeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.english())
    }
}

/// Why a file couldn't be read: Telescope can't use what's in it, or can't open it at all.
#[derive(Debug)]
pub enum ReadError {
    Cube(CubeError),
    Io(std::io::Error),
}

/// A LUT read from a .cube file: a 1D table, a 3D cube, or a 1D shaper followed by a cube.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CubeLut {
    pub title: String,
    /// Output RGB per input step (n triples), for inputs from shaper_min to shaper_max.
    pub shaper: Option<Vec<f32>>,
    pub shaper_min: [f32; 3],
    pub shaper_max: [f32; 3],
    /// size³ RGB triples [b][g][r], for inputs from cube_min to cube_max.
    pub cube: Option<Vec<f32>>,
    pub cube_size: usize,
    pub cube_min: [f32; 3],
    pub cube_max: [f32; 3],
}

impl CubeLut {
    /// A 3D LUT over 0..1.
    pub fn from_cube(rgb: Vec<f32>, size: usize) -> CubeLut {
        CubeLut { cube: Some(rgb), cube_size: size, shaper_max: [1.0; 3], cube_max: [1.0; 3], ..Default::default() }
    }

    /// A 1D LUT over 0..1.
    pub fn from_shaper(rgb: Vec<f32>) -> CubeLut {
        CubeLut { shaper: Some(rgb), shaper_max: [1.0; 3], cube_max: [1.0; 3], ..Default::default() }
    }

    /// Points per side of its cube, or entries of its table without one.
    pub fn size(&self) -> usize {
        if self.cube.is_some() { self.cube_size } else { self.shaper.as_ref().map_or(0, |s| s.len() / 3) }
    }

    /// No 3D part: each channel's output depends on that channel's input alone.
    pub fn per_channel(&self) -> bool {
        self.cube.is_none()
    }

    /// Colours 0..1 through it.
    pub fn eval(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mut out = rgb;
        if let Some(shaper) = &self.shaper {
            let n = shaper.len() / 3;
            for c in 0..3 {
                let t = (out[c] - self.shaper_min[c]) / (self.shaper_max[c] - self.shaper_min[c]).max(1e-12);
                // np.interp: flat past either end
                let pos = t.clamp(0.0, 1.0) * (n - 1) as f32;
                let lo = (pos as usize).min(n.saturating_sub(2));
                let k = pos - lo as f32;
                out[c] = if n < 2 { shaper[c] } else { shaper[lo * 3 + c] * (1.0 - k) + shaper[(lo + 1) * 3 + c] * k };
            }
        }
        if let Some(cube) = &self.cube {
            let mut x = [0.0f32; 3];
            for c in 0..3 {
                x[c] = (out[c] - self.cube_min[c]) / (self.cube_max[c] - self.cube_min[c]).max(1e-12);
            }
            out = sample_cube(cube, self.cube_size, x);
        }
        out
    }
}

fn is_number(token: &str) -> bool {
    // [-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?
    let b = token.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - start
    };
    let whole = digits(&mut i);
    if whole > 0 {
        if i < b.len() && b[i] == b'.' {
            i += 1;
            digits(&mut i);
        }
    } else if i < b.len() && b[i] == b'.' {
        i += 1;
        if digits(&mut i) == 0 {
            return false;
        }
    } else {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

/// Whitespace as Python's str.split() and strip() see it.
fn is_space(c: char) -> bool {
    c.is_whitespace()
}

/// line up to a # that isn't inside quotes (a TITLE can have one).
fn without_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

fn triple(line: &str) -> Option<[f64; 3]> {
    let words: Vec<&str> = line.split(is_space).filter(|w| !w.is_empty()).collect();
    if words.len() != 3 || !words.iter().all(|w| is_number(w)) {
        return None;
    }
    Some([words[0].parse().ok()?, words[1].parse().ok()?, words[2].parse().ok()?])
}

fn floats(rest: &str) -> Option<Vec<f64>> {
    rest.split(is_space).filter(|w| !w.is_empty()).map(|w| w.parse::<f64>().ok()).collect()
}

/// str.splitlines(): a line ends at \n, \r\n or a bare \r, and at \v, \f, the file, group and record separators, NEL and the Unicode
/// line and paragraph separators.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let (mut start, mut chars) = (0, text.char_indices().peekable());
    while let Some((at, c)) = chars.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            lines.push(&text[start..at]);
            start = at + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
                chars.next();
                start += 1;
            }
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// A .cube file's text (Adobe's format, plus Resolve's LUT_1D_INPUT_RANGE and shaper-then-cube files).
pub fn parse_cube(text: &str) -> Result<CubeLut, CubeError> {
    let (mut title, mut size1, mut size3) = (String::new(), 0i64, 0i64);
    let (mut dmin, mut dmax) = ([0.0f64; 3], [1.0f64; 3]);
    let (mut range1, mut range3): (Option<(f64, f64)>, Option<(f64, f64)>) = (None, None);
    let mut rows: Vec<[f64; 3]> = Vec::new();
    for (i, raw) in split_lines(text).into_iter().enumerate() {
        let number = i + 1;
        let line = without_comment(raw).trim_matches(is_space);
        if line.is_empty() {
            continue;
        }
        if let Some(t) = triple(line) {
            rows.push(t);
            continue;
        }
        let (word, rest) = match line.split_once(is_space) {
            Some((w, r)) => (w, r.trim_matches(is_space)),
            None => (line, ""),
        };
        let word = word.to_uppercase();
        let bad = || CubeError::new(BAD_LINE, &[("line", number.to_string())]);
        match word.as_str() {
            "TITLE" => title = rest.trim_matches('"').to_string(),
            "LUT_1D_SIZE" => size1 = rest.parse().map_err(|_| bad())?,
            "LUT_3D_SIZE" => size3 = rest.parse().map_err(|_| bad())?,
            "DOMAIN_MIN" | "DOMAIN_MAX" => {
                let v = floats(rest).filter(|v| v.len() == 3).ok_or_else(bad)?;
                let v = [v[0], v[1], v[2]];
                if word == "DOMAIN_MIN" { dmin = v } else { dmax = v }
            }
            "LUT_1D_INPUT_RANGE" | "LUT_3D_INPUT_RANGE" => {
                let v = floats(rest).filter(|v| v.len() == 2).ok_or_else(bad)?;
                if word == "LUT_1D_INPUT_RANGE" { range1 = Some((v[0], v[1])) } else { range3 = Some((v[0], v[1])) }
            }
            // other keywords before the data (LUT_IN_VIDEO_RANGE and the like) don't change the maths
            _ if !rows.is_empty() => return Err(CubeError::new(NOT_A_COLOUR, &[("line", number.to_string())])),
            _ => {}
        }
    }
    if size1 == 0 && size3 == 0 {
        return Err(CubeError::new(NO_SIZE, &[]));
    }
    if size3 != 0 && !(2..=MAX_CUBE_SIZE as i64).contains(&size3) {
        return Err(CubeError::new(TOO_MANY_POINTS, &[("size", size3.to_string()), ("most", MAX_CUBE_SIZE.to_string())]));
    }
    if size1 != 0 && !(2..=65536).contains(&size1) {
        return Err(CubeError::new(BAD_1D_SIZE, &[("size", size1.to_string())]));
    }
    let want = size1 + size3 * size3 * size3;
    if rows.len() as i64 != want {
        return Err(CubeError::new(WRONG_COUNT, &[("expected", want.to_string()), ("found", rows.len().to_string())]));
    }
    let data: Vec<f32> = rows.iter().flatten().map(|v| *v as f32).collect();
    if !data.iter().all(|v| v.is_finite()) {
        return Err(CubeError::new(NOT_NUMBERS, &[]));
    }
    // The domains are checked as the f64 they were read as; only what is kept is f32
    let both = |(lo, hi): (f64, f64)| ([lo; 3], [hi; 3]);
    let (shaper_min, shaper_max) = range1.map_or((dmin, dmax), both);
    let (mut cube_min, mut cube_max) = range3.map_or((dmin, dmax), both);
    let (size1, size3) = (size1 as usize, size3 as usize);
    let mut shaper = None;
    let mut cube = None;
    if size1 > 0 {
        shaper = Some(data[..size1 * 3].to_vec());
        if size3 > 0 {
            // Resolve's shaper-then-cube files give the cube its own range, or 0..1
            (cube_min, cube_max) = range3.map_or(([0.0; 3], [1.0; 3]), both);
        }
    }
    if size3 > 0 {
        cube = Some(data[size1 * 3..].to_vec());
    }
    for (lo, hi) in [(shaper_min, shaper_max), (cube_min, cube_max)] {
        if (0..3).any(|c| hi[c] <= lo[c]) {
            return Err(CubeError::new(BAD_DOMAIN, &[]));
        }
    }
    let v3 = |v: [f64; 3]| v.map(|x| x as f32);
    Ok(CubeLut { title, shaper, shaper_min: v3(shaper_min), shaper_max: v3(shaper_max), cube, cube_size: size3, cube_min: v3(cube_min), cube_max: v3(cube_max) })
}

/// A .cube file; an error when it can't be used (or can't be read at all).
pub fn read_cube(path: &Path) -> Result<CubeLut, ReadError> {
    let meta = std::fs::metadata(path).map_err(ReadError::Io)?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(ReadError::Cube(CubeError::new(TOO_BIG, &[])));
    }
    let bytes = std::fs::read(path).map_err(ReadError::Io)?;
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    parse_cube(&String::from_utf8_lossy(bytes)).map_err(ReadError::Cube)
}

/// A 3D LUT (size³ RGB triples [b][g][r]) as .cube text: domain 0..1, red changing fastest.
pub fn cube_text(cube: &[f32], size: usize, title: &str) -> String {
    let mut lines = Vec::with_capacity(cube.len() / 3 + 6);
    if !title.is_empty() {
        lines.push(format!("TITLE \"{}\"", title.replace('"', "'")));
    }
    lines.extend(["# Made with Telescope".to_string(), format!("LUT_3D_SIZE {size}"), "DOMAIN_MIN 0.0 0.0 0.0".into(), "DOMAIN_MAX 1.0 1.0 1.0".into()]);
    for px in cube.chunks_exact(3) {
        let v = |i: usize| px[i].clamp(0.0, 1.0);
        lines.push(format!("{:.6} {:.6} {:.6}", v(0), v(1), v(2)));
    }
    lines.join("\n") + "\n"
}

/// Writes a 3D LUT as a .cube file, through a temporary file so a failed write leaves no half file.
pub fn write_cube(path: &Path, cube: &[f32], size: usize, title: &str) -> std::io::Result<()> {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    let temp = path.with_file_name(name);
    std::fs::write(&temp, cube_text(cube, size, title))?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::colour::lattice;
    use telescope_core::testing::scratch_dir;

    fn rows(size: usize, f: impl Fn(usize, usize, usize) -> String) -> String {
        let mut out = Vec::new();
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    out.push(f(r, g, b));
                }
            }
        }
        out.join("\n")
    }

    fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        (0..3).all(|i| (a[i] - b[i]).abs() <= tol)
    }

    #[test]
    fn a_3d_cube_reads_red_fastest() {
        let body = rows(2, |r, g, b| format!("{r} {g} {b}"));
        let lut = parse_cube(&format!("TITLE \"Plain\"\n# a comment\nLUT_3D_SIZE 2\n{body}\n")).unwrap();
        assert!(lut.title == "Plain" && lut.size() == 2 && !lut.per_channel());
        // [b][g][r]
        let at = (1 * 2 * 2) * 3;
        assert_eq!(&lut.cube.as_ref().unwrap()[at..at + 3], &[0.0, 0.0, 1.0]);
        assert!(close(lut.eval([0.25, 0.5, 0.75]), [0.25, 0.5, 0.75], 1e-6));
    }

    #[test]
    fn a_1d_lut_with_its_own_domain() {
        let lut = parse_cube("TITLE \"Demo\"\nLUT_1D_SIZE 3\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 2 3\n0 0 0\n# here too\n0.5 1 1.5\n1 1 1\n").unwrap();
        assert!(lut.per_channel() && lut.size() == 3);
        assert!(close(lut.eval([0.5, 0.5, 0.5]), [0.5, 0.5, 0.5], 1e-6), "{:?}", lut.eval([0.5, 0.5, 0.5]));
    }

    #[test]
    fn a_resolve_shaper_then_cube() {
        let body = rows(2, |r, g, b| format!("{r} {g} {b}"));
        let lut = parse_cube(&format!("LUT_1D_SIZE 2\nLUT_1D_INPUT_RANGE 0 2\nLUT_3D_SIZE 2\nLUT_3D_INPUT_RANGE 0 2\n0 0 0\n2 2 2\n{body}")).unwrap();
        assert_eq!((lut.shaper_max, lut.cube_max), ([2.0; 3], [2.0; 3]));
        // 0.5 through the shaper (0..2 onto 0..2) stays 0.5, then the cube (0..2) gives 0.25
        assert!(close(lut.eval([0.5, 0.5, 0.5]), [0.25, 0.25, 0.25], 1e-6));
    }

    #[test]
    fn files_telescope_cant_use_say_why() {
        for (text, problem) in [
            ("TITLE x\n0 0 0\n", "doesn't say its size"),
            ("LUT_3D_SIZE 129\n", "129 points per side"),
            ("LUT_3D_SIZE 2\n0 0 0\n", "should have 8 colours but has 1"),
            ("LUT_1D_SIZE 2\n0 0 0\nnope\n1 1 1\n", "Line 3"),
            ("LUT_1D_SIZE 2\nDOMAIN_MIN 1 1 1\nDOMAIN_MAX 0 0 0\n0 0 0\n1 1 1\n", "DOMAIN_MAX"),
            ("LUT_1D_SIZE x\n", "Line 1"),
            ("LUT_1D_SIZE 2\n0 0 0\n1 nan 1\n", "Line 3"),
        ] {
            let err = parse_cube(text).unwrap_err();
            assert!(err.english().contains(problem), "{text:?}: {err}");
            assert!(!err.text.is_empty(), "the untranslated text the card shows through tr()");
        }
    }

    #[test]
    fn tabs_after_keywords_and_a_hash_inside_a_quoted_title() {
        let body = rows(2, |r, g, b| format!("{r}\t{g}\t{b}"));
        let lut = parse_cube(&format!("TITLE\t\"Look # 2\" # a comment\nLUT_3D_SIZE\t2\nDOMAIN_MAX\t1\t1\t1\n{body}")).unwrap();
        assert!(lut.title == "Look # 2" && lut.size() == 2);
    }

    #[test]
    fn a_file_that_starts_with_a_byte_order_mark() {
        let dir = scratch_dir("cube-bom");
        let path = dir.join("Bom.cube");
        std::fs::write(&path, b"\xef\xbb\xbfLUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
        assert_eq!(read_cube(&path).unwrap().size(), 2);
    }

    #[test]
    fn a_cube_written_reads_back_the_same() {
        let dir = scratch_dir("cube-round");
        let mut cube = lattice(17);
        for px in cube.chunks_exact_mut(3) {
            let (r, g, b) = (px[0], px[1], px[2]);
            px.copy_from_slice(&[r.powf(0.8), g * 0.9 + b * 0.1, b.sqrt()]);
        }
        let path = dir.join("Look \u{f6}.cube");
        write_cube(&path, &cube, 17, "A \"quoted\" title").unwrap();
        let back = read_cube(&path).unwrap();
        assert!(back.title == "A 'quoted' title" && back.size() == 17);
        let worst = back.cube.unwrap().iter().zip(&cube).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "{worst}");
        assert!(std::fs::read_dir(&dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".part")));
    }

    #[test]
    fn a_huge_file_is_turned_away() {
        let dir = scratch_dir("cube-huge");
        let path = dir.join("big.cube");
        std::fs::File::create(&path).unwrap().set_len(65 * 1024 * 1024).unwrap();
        assert!(matches!(read_cube(&path), Err(ReadError::Cube(e)) if e.text == TOO_BIG));
        assert!(matches!(read_cube(&dir.join("nope.cube")), Err(ReadError::Io(_))));
    }

    #[test]
    fn real_cube_files_load_and_survive_a_round_trip() {
        // TELESCOPE_CUBE_DIR names a folder of real .cube files to check; without it there's nothing to look at
        let Some(folder) = std::env::var_os("TELESCOPE_CUBE_DIR") else { return };
        let mut files = Vec::new();
        let mut todo = vec![std::path::PathBuf::from(folder)];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() { todo.push(p) } else if p.extension().is_some_and(|x| x == "cube") { files.push(p) }
            }
        }
        assert!(!files.is_empty());
        let out_dir = scratch_dir("cube-real");
        for path in files {
            let lut = read_cube(&path).unwrap_or_else(|_| panic!("{path:?}"));
            let looked: Vec<f32> = lattice(9).chunks_exact(3).flat_map(|p| lut.eval([p[0], p[1], p[2]])).collect();
            assert!(looked.len() == 9 * 9 * 9 * 3 && looked.iter().all(|v| v.is_finite()), "{path:?}");
            let cube: Vec<f32> = lattice(33).chunks_exact(3).flat_map(|p| lut.eval([p[0], p[1], p[2]]).map(|v| v.clamp(0.0, 1.0))).collect();
            let out = out_dir.join(path.file_name().unwrap());
            write_cube(&out, &cube, 33, &lut.title).unwrap();
            let back = read_cube(&out).unwrap();
            let worst = back.cube.unwrap().iter().zip(&cube).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst < 1e-6, "{path:?}");
        }
    }

    #[test]
    fn lines_end_the_way_pythons_splitlines_ends_them() {
        // an old Mac file: nothing but \r between the lines
        let lut = parse_cube("LUT_1D_SIZE 2\r0 0 0\r1 1 1\r").unwrap();
        assert!(lut.per_channel() && lut.size() == 2);
        assert_eq!(split_lines("a\r\nb\rc\nd\u{2028}e\x0cf"), ["a", "b", "c", "d", "e", "f"]);
        assert_eq!(split_lines("a\n\nb\n"), ["a", "", "b"]);
    }

    #[test]
    fn a_domain_is_checked_before_it_is_squeezed_into_a_single_float() {
        // 1.00000001 is above 1 as a double, though the same float
        let lut = parse_cube("LUT_1D_SIZE 2\nDOMAIN_MIN 1 1 1\nDOMAIN_MAX 1.00000001 1.00000001 1.00000001\n0 0 0\n1 1 1\n");
        assert!(lut.is_ok());
        assert!(parse_cube("LUT_1D_SIZE 2\nDOMAIN_MIN 1 1 1\nDOMAIN_MAX 1 1 1\n0 0 0\n1 1 1\n").is_err());
    }
}
