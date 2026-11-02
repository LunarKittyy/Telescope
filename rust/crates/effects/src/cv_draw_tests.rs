use super::*;

const RECORDED: &str = include_str!("../tests/fixtures/cv_draw.txt");

fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

/// Every case is a cv2 call recorded by tests/fixtures/make_cv_draw.py: the same call here, the same bytes.
#[test]
fn drawing_matches_opencv_byte_for_byte() {
    let mut cases = 0;
    let mut wrong = Vec::new();
    for line in RECORDED.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let n = |i: usize| f[i].parse::<i32>().unwrap();
        let (w, h, channels) = (n(1) as usize, n(2) as usize, n(3) as usize);
        let mut data = vec![n(4) as u8; w * h * channels];
        let colour = [n(5) as u8, n(6) as u8, n(7) as u8, n(8) as u8];
        let mut canvas = Canvas { data: &mut data, w, h, channels };
        let expected = u64::from_str_radix(f[f.len() - 1], 16).unwrap();
        match f[0] {
            "line" => canvas.line((n(9), n(10)), (n(11), n(12)), colour, n(13)),
            "circle" => canvas.circle((n(9), n(10)), n(11), colour, n(12)),
            "text" => {
                let scale = plain_scale_from_height(n(9));
                let c = char::from_u32(n(12) as u32).unwrap().to_string();
                assert_eq!(plain_text_width(&c, scale), n(13), "{line}");
                canvas.plain_text(&c, (n(10), n(11)), scale, colour);
            }
            other => panic!("{other}"),
        }
        cases += 1;
        if fnv(&data) != expected {
            wrong.push(line.to_string());
        }
    }
    assert!(cases > 300, "{cases}");
    assert!(wrong.is_empty(), "{} of {cases} differ, first: {:?}", wrong.len(), &wrong[..wrong.len().min(8)]);
}
