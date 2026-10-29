//! QR codes to show, as python-qrcode draws them (widgets/qr.py): error correction L, the same split into modes and the mask python-qrcode
//! scores best, so the code looks the same as the Qt app's. Pairing, the first-run checklist and the On Air sign all show one.

use qrcode::canvas::{Canvas, MaskPattern};
use qrcode::{bits::Bits, EcLevel, Version};

/// The QR code's dark cells, as indexes row by row, and its size in modules (widgets/qr.py, error correction L). The mask
/// is the one python-qrcode picks, so the code looks the same as the Qt app's.
pub fn encode(data: &str) -> (Vec<usize>, usize) {
    let chunks = chunks(data.as_bytes());
    // fit=True: the smallest version the chunks fit in
    let fitted = (1..=40).find_map(|v| {
        let version = Version::Normal(v);
        let mut bits = Bits::new(version);
        for (mode, bytes) in &chunks {
            match mode {
                Mode::Numeric => bits.push_numeric_data(bytes),
                Mode::Alphanumeric => bits.push_alphanumeric_data(bytes),
                Mode::Bytes => bits.push_byte_data(bytes),
            }.ok()?;
        }
        bits.push_terminator(EcLevel::L).ok()?;
        let (codewords, ec) = qrcode::ec::construct_codewords(&bits.into_bytes(), version, EcLevel::L).ok()?;
        Some((version, codewords, ec))
    });
    let Some((version, codewords, ec)) = fitted else { return (Vec::new(), 0) };
    let mut canvas = Canvas::new(version, EcLevel::L);
    canvas.draw_all_functional_patterns();
    canvas.draw_data(&codewords, &ec);
    let n = version.width() as usize;
    let masks = [MaskPattern::Checkerboard, MaskPattern::HorizontalLines, MaskPattern::VerticalLines, MaskPattern::DiagonalLines,
                 MaskPattern::LargeCheckerboard, MaskPattern::Fields, MaskPattern::Diamonds, MaskPattern::Meadow];
    let matrix = |mask: MaskPattern| -> Vec<Vec<bool>> {
        let mut c = canvas.clone();
        c.apply_mask(mask);
        (0..n).map(|row| (0..n).map(|col| c.get(col as i16, row as i16).is_dark()).collect()).collect()
    };
    // best_mask_pattern: each mask scored with the format bits and the fixed dark module left light (makeImpl(True, i))
    let mut best = (usize::MAX, 0);
    for (i, &mask) in masks.iter().enumerate() {
        let mut m = matrix(mask);
        for k in 0..15 {
            let row = if k < 6 { k } else if k < 8 { k + 1 } else { n - 15 + k };
            m[row][8] = false;
            let col = if k < 8 { n - k - 1 } else if k < 9 { 15 - k } else { 14 - k };
            m[8][col] = false;
        }
        m[n - 8][8] = false;
        let lost = lost_point(&m);
        if lost < best.0 {
            best = (lost, i);
        }
    }
    let m = matrix(masks[best.1]);
    ((0..n * n).filter(|&i| m[i / n][i % n]).collect(), n)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Numeric,
    Alphanumeric,
    Bytes,
}

/// What QR alphanumeric mode takes besides digits and capitals.
const ALPHANUMERIC: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";

/// Runs of at least `minimum` bytes that all satisfy `is`, as (is a run, bytes) in order (python-qrcode's _optimal_split with
/// the pattern `[class]{minimum,}`).
fn split<'a>(data: &'a [u8], minimum: usize, is: &dyn Fn(u8) -> bool) -> Vec<(bool, &'a [u8])> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let found = (0..rest.len()).find(|&start| rest.len() - start >= minimum && rest[start..start + minimum].iter().all(|b| is(*b)));
        let Some(start) = found else { break };
        let end = start + rest[start..].iter().take_while(|b| is(**b)).count();
        if start > 0 {
            out.push((false, &rest[..start]));
        }
        out.push((true, &rest[start..end]));
        rest = &rest[end..];
    }
    if !rest.is_empty() {
        out.push((false, rest));
    }
    out
}

/// util.optimal_data_chunks(data, minimum=20), which QRCode.add_data does by default: runs of 20 or more digits go in numeric mode
/// and runs of 20 or more characters QR's alphanumeric mode has in that, the rest as bytes. Data that short as a whole is one
/// chunk in the most compact mode it fits.
fn chunks(data: &[u8]) -> Vec<(Mode, &[u8])> {
    const MINIMUM: usize = 20;
    let digit = |b: u8| b.is_ascii_digit();
    let alnum = |b: u8| ALPHANUMERIC.contains(&b);
    if data.len() <= MINIMUM {
        if data.is_empty() {
            return vec![(Mode::Bytes, data)];
        }
        let mode = if data.iter().all(|b| digit(*b)) { Mode::Numeric } else if data.iter().all(|b| alnum(*b)) { Mode::Alphanumeric } else { Mode::Bytes };
        return vec![(mode, data)];
    }
    let mut out = Vec::new();
    for (numeric, chunk) in split(data, MINIMUM, &digit) {
        if numeric {
            out.push((Mode::Numeric, chunk));
            continue;
        }
        for (alpha, sub) in split(chunk, MINIMUM, &alnum) {
            out.push((if alpha { Mode::Alphanumeric } else { Mode::Bytes }, sub));
        }
    }
    out
}

/// python-qrcode's util.lost_point: the penalty a mask is chosen by.
fn lost_point(m: &[Vec<bool>]) -> usize {
    let n = m.len();
    let at = |row: usize, col: usize, transposed: bool| if transposed { m[col][row] } else { m[row][col] };
    let mut lost = 0;
    // Runs of five or more of one colour, in rows and columns
    for transposed in [false, true] {
        for row in 0..n {
            let (mut prev, mut len) = (at(row, 0, transposed), 0);
            for col in 0..n {
                if at(row, col, transposed) == prev {
                    len += 1;
                } else {
                    if len >= 5 {
                        lost += len - 2;
                    }
                    len = 1;
                    prev = at(row, col, transposed);
                }
            }
            if len >= 5 {
                lost += len - 2;
            }
        }
    }
    // 2x2 blocks of one colour, skipping as python-qrcode does
    for row in 0..n - 1 {
        let mut col = 0;
        while col < n - 1 {
            let top_right = m[row][col + 1];
            if top_right != m[row + 1][col + 1] {
                col += 1;
            } else if top_right == m[row][col] && top_right == m[row + 1][col] {
                lost += 3;
            }
            col += 1;
        }
    }
    // Finder-like 1:1:3:1:1 runs next to four light modules
    for transposed in [false, true] {
        for row in 0..n {
            let mut col = 0;
            while col + 10 < n {
                let v = |k: usize| at(row, col + k, transposed);
                if !v(1) && v(4) && !v(5) && v(6) && !v(9)
                    && (v(0) && v(2) && v(3) && !v(7) && !v(8) && !v(10) || !v(0) && !v(2) && !v(3) && v(7) && v(8) && v(10)) {
                    lost += 40;
                }
                if v(10) {
                    col += 1;
                }
                col += 1;
            }
        }
    }
    let dark = m.iter().flatten().filter(|d| **d).count();
    lost + ((dark as f64 / (n * n) as f64 * 100.0 - 50.0).abs() / 5.0) as usize * 10
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(data: &str) -> Vec<String> {
        let (cells, size) = encode(data);
        (0..size).map(|r| (0..size).map(|c| if cells.contains(&(r * size + c)) { '#' } else { '.' }).collect()).collect()
    }

    #[test]
    fn the_code_is_the_one_python_qrcode_draws() {
        // python-qrcode's modules for the scenes' payload, mask chosen by its best_mask_pattern
        let want = [
                "#######.####.##...#######",
                "#.....#....###..#.#.....#",
                "#.###.#..#.#.##...#.###.#",
                "#.###.#...##.#.##.#.###.#",
                "#.###.#...####....#.###.#",
                "#.....#..#.#....#.#.....#",
                "#######.#.#.#.#.#.#######",
                "........##..####.........",
                "##.##.#..#..#.##..#.....#",
                "###..#.##.#...##.#..##...",
                "#.###.##.#..#.####.###..#",
                "#.#.#..#......#.##.#.##.#",
                "...########.....####.#..#",
                "##..##.#.##.#.###...##...",
                "##.#..##.#...###.#.#..###",
                "#.####..##..#...##..###.#",
                "#..#.##.#.####.######.#.#",
                "........#.##..#.#...#.#..",
                "#######.....###.#.#.#...#",
                "#.....#...##.####...##..#",
                "#.###.#.#.###.########...",
                "#.###.#.#.#.###.#.##..###",
                "#.###.#....#...#.#.##.###",
                "#.....#.#.##...#...######",
                "#######.#..##...#....#..#",
        ];
        assert_eq!(rows("telescope://pair/example"), want);
    }

    #[test]
    fn a_link_with_a_run_of_alphanumerics_is_split_into_modes() {
        // The On Air sign's link as python-qrcode draws it: its middle goes in alphanumeric mode
        let want = [
            "#######.##.##.##..#...#######",
            "#.....#.##...#.#...#..#.....#",
            "#.###.#..##.#...##.##.#.###.#",
            "#.###.#.......##......#.###.#",
            "#.###.#.##..##.#....#.#.###.#",
            "#.....#.#..#.#..##.##.#.....#",
            "#######.#.#.#.#.#.#.#.#######",
            "........#..#...#.#..#........",
            "###..##.###...##.###.####..##",
            "#.###...##.###.#.#..##..#..##",
            "..#.###.#..##.###.#.###.###.#",
            "..####.#.#.#.###..##.#...#...",
            "#.#####..#.###.....##.#.....#",
            "###....#...#.#.#.#.#..#.#..##",
            "#...#.##......#.####........#",
            "..##...##.#....#.####.###....",
            ".#..#.##..#......#..#...##..#",
            ".#####.#.##..#.#.#.##..#...##",
            "##.#..##.###..#.#.##..##.##.#",
            "..##...###.#.###.##..###..#..",
            "##....###..#.#.#..########.#.",
            "........##..#.##.#..#...###.#",
            "#######....#..##..#.#.#.#...#",
            "#.....#.####.#.#.#.##...####.",
            "#.###.#...##.##.##.#######..#",
            "#.###.#..#####....##.##..#...",
            "#.###.#.#.#####...###.###..##",
            "#.....#.##.#.##..#....#.###..",
            "#######.##..###..####.###...#",
        ];
        assert_eq!(rows("https://192.168.1.20:8768/#Qm9uZS1fWWFiYzEyMzQ1Njc4OTBB"), want);
    }

    #[test]
    fn a_long_run_of_digits_goes_in_numeric_mode() {
        let want = [
            "#######.###.###...#######.#######",
            "#.....#.###..###..#.#.##..#.....#",
            "#.###.#.#.##.####.#....##.#.###.#",
            "#.###.#.##.#.#.......#.##.#.###.#",
            "#.###.#........#..#.##.##.#.###.#",
            "#.....#.#..###.##......#..#.....#",
            "#######.#.#.#.#.#.#.#.#.#.#######",
            ".........#..####..#.#..#.........",
            "##..###....#..###...#...#..#.####",
            "#...#..####.#..#...#..#.#.##.###.",
            "...##.#....##....#.###.....###.#.",
            "##.##.....##.###....#.##.........",
            "#..####.#.#.###...##.....#.##..##",
            "####...#.#...#.#..##....#...##..#",
            "#.###.####....#..#.####.#.#.#.#..",
            "###.#...#...#..#..#....#.#.###.##",
            ".#.######.##.###...##.##...######",
            "##.#......#.#.#.#.##..###.###...#",
            "###...#########..#.####.#..#..##.",
            "...###.#...#.#.##.#....###..#.#..",
            "...##.####..##.#..###...##....#.#",
            "####.#..#....##.#.###.#.#########",
            "..##.####.#......###.#..##...###.",
            "...##..##.#.#.##..#.#..#.#.#..##.",
            "###.#.####.#.##.#....##.######.#.",
            "........##..#....##.#...#...#.###",
            "#######..#.####..########.#.#..#.",
            "#.....#.##.#.#.##.#....##...#####",
            "#.###.#.##..##.##..#..########.##",
            "#.###.#...#..##.#..#..#.##.#.#.##",
            "#.###.#..#.......#.#####..####...",
            "#.....#.##..#.###.....#.##.......",
            "#######.####.##...###.....###...#",
        ];
        assert_eq!(rows(&format!("IDS{}tail and more text to push it past version one", "1".repeat(25))), want);
    }

    #[test]
    fn data_is_split_the_way_python_qrcode_does() {
        assert_eq!(chunks(b"12345"), vec![(Mode::Numeric, &b"12345"[..])]);
        assert_eq!(chunks(b"HELLO-123"), vec![(Mode::Alphanumeric, &b"HELLO-123"[..])]);
        assert_eq!(chunks(b"hello"), vec![(Mode::Bytes, &b"hello"[..])]);
        let long = b"https://192.168.1.20:8768/#abc";
        assert_eq!(chunks(long), vec![(Mode::Bytes, &b"https"[..]), (Mode::Alphanumeric, &b"://192.168.1.20:8768/"[..]), (Mode::Bytes, &b"#abc"[..])]);
        // Nineteen digits in a row are too few to split off
        assert_eq!(chunks(b"abcdefghij1234567890123456789"), vec![(Mode::Bytes, &b"abcdefghij1234567890123456789"[..])]);
    }

    #[test]
    fn nothing_that_wont_fit_is_drawn() {
        assert_eq!(encode(&"x".repeat(4000)), (Vec::new(), 0));
    }
}
