//! OpenCV's anti-aliased lines, circles and Hershey Plain text (drawing.cpp), exact to the byte. The picture may be 1, 3 or 4 channels;
//! a colour's channels are in the picture's own order.

const XY_SHIFT: i64 = 16;
const XY_ONE: i64 = 1 << XY_SHIFT;

const FILTER: [i32; 64] = [
    168, 177, 185, 194, 202, 210, 218, 224, 231, 236, 241, 246, 249, 252, 254, 254, 254, 254, 252, 249, 246, 241, 236, 231, 224, 218, 210, 202, 194, 185, 177, 168, 158, 149, 140, 131, 122, 114,
    105, 97, 89, 82, 75, 68, 62, 56, 50, 45, 40, 36, 32, 28, 25, 22, 19, 16, 14, 12, 11, 9, 8, 7, 5, 5,
];
const SLOPE_CORR: [i64; 32] = [181, 181, 181, 182, 182, 183, 184, 185, 187, 188, 190, 192, 194, 196, 198, 201, 203, 206, 209, 211, 214, 218, 221, 224, 227, 231, 235, 238, 242, 246, 250, 254];

/// OpenCV's table of sines holds seven decimals as floats, so the circles it draws land where this does and not where sin() would.
fn sin_table(degrees: i64) -> f64 {
    (((degrees as f64).to_radians().sin() * 1e7).round() / 1e7) as f32 as f64
}

fn cv_round(v: f64) -> i64 {
    v.round_ties_even() as i64
}

#[derive(Clone, Copy, PartialEq)]
struct Pt {
    x: i64,
    y: i64,
}

/// A picture to draw on, rows packed.
pub struct Canvas<'a> {
    pub data: &'a mut [u8],
    pub w: usize,
    pub h: usize,
    pub channels: usize,
}

impl Canvas<'_> {
    /// cv2.line with LINE_AA.
    pub fn line(&mut self, from: (i32, i32), to: (i32, i32), colour: [u8; 4], thickness: i32) {
        self.thick_line(Pt { x: from.0 as i64, y: from.1 as i64 }, Pt { x: to.0 as i64, y: to.1 as i64 }, colour, thickness, 3, 0);
    }

    /// cv2.circle with LINE_AA.
    pub fn circle(&mut self, centre: (i32, i32), radius: i32, colour: [u8; 4], thickness: i32) {
        let c = Pt { x: (centre.0 as i64) << XY_SHIFT, y: (centre.1 as i64) << XY_SHIFT };
        self.ellipse(c, (radius as i64) << XY_SHIFT, colour, thickness);
    }

    /// A one pixel line along a row or a column, as cv2.line draws one with its default LINE_8.
    pub fn straight_line(&mut self, from: (i32, i32), to: (i32, i32), colour: [u8; 4]) {
        let steps = (to.0 - from.0).abs().max((to.1 - from.1).abs());
        let (dx, dy) = ((to.0 - from.0).signum(), (to.1 - from.1).signum());
        for i in 0..=steps {
            self.put(from.0 + dx * i, from.1 + dy * i, colour);
        }
    }

    /// cv2.rectangle with thickness 1 and LINE_8.
    pub fn rectangle(&mut self, from: (i32, i32), to: (i32, i32), colour: [u8; 4]) {
        self.straight_line((from.0, from.1), (to.0, from.1), colour);
        self.straight_line((to.0, from.1), (to.0, to.1), colour);
        self.straight_line((to.0, to.1), (from.0, to.1), colour);
        self.straight_line((from.0, to.1), (from.0, from.1), colour);
    }

    /// cv2.putText in FONT_HERSHEY_PLAIN with LINE_AA and thickness 1, the text's baseline at `origin`.
    pub fn plain_text(&mut self, text: &str, origin: (i32, i32), scale: f64, colour: [u8; 4]) {
        let hscale = cv_round(scale * XY_ONE as f64);
        let mut view_x = (origin.0 as i64) << XY_SHIFT;
        let view_y = ((origin.1 as i64) << XY_SHIFT) - PLAIN_BASE * hscale;
        for c in text.chars() {
            let Some(glyph) = plain_glyph(c) else { continue };
            let bytes = glyph.as_bytes();
            let (left, right) = (bytes[0] as i64 - b'R' as i64, bytes[1] as i64 - b'R' as i64);
            view_x -= left * hscale;
            for stroke in glyph[2..].split(' ') {
                let pts: Vec<Pt> = stroke.as_bytes().as_chunks::<2>().0.iter().map(|p| Pt { x: (p[0] as i64 - b'R' as i64) * hscale + view_x, y: (p[1] as i64 - b'R' as i64) * hscale + view_y }).collect();
                if pts.len() > 1 {
                    self.poly_line(&pts, colour, 1);
                }
            }
            view_x += right * hscale;
        }
    }

    fn put(&mut self, x: i32, y: i32, colour: [u8; 4]) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            let i = (y as usize * self.w + x as usize) * self.channels;
            self.data[i..i + self.channels].copy_from_slice(&colour[..self.channels]);
        }
    }

    fn blend(&mut self, x: i64, y: i64, colour: [u8; 4], a: i32) {
        let i = (y as usize * self.w + x as usize) * self.channels;
        for (c, &target) in colour.iter().enumerate().take(self.channels) {
            let mut v = self.data[i + c] as i32;
            v += ((target as i32 - v) * a + 127) >> 8;
            v += ((target as i32 - v) * a + 127) >> 8;
            self.data[i + c] = v as u8;
        }
    }

    fn line_aa(&mut self, mut p1: Pt, mut p2: Pt, colour: [u8; 4]) {
        if !clip_line(((self.w as i64) << XY_SHIFT, (self.h as i64) << XY_SHIFT), &mut p1, &mut p2) {
            return;
        }
        let (mut dx, mut dy) = (p2.x - p1.x, p2.y - p1.y);
        let j = if dx < 0 { -1i64 } else { 0 };
        let ax = (dx ^ j) - j;
        let i = if dy < 0 { -1i64 } else { 0 };
        let ay = (dy ^ i) - i;
        let along_x = ax > ay;
        let (x_step, y_step, mut ecount, slope, start, end);
        if along_x {
            dy = (dy ^ j) - j;
            if j != 0 {
                std::mem::swap(&mut p1, &mut p2);
            }
            x_step = XY_ONE;
            y_step = (dy << XY_SHIFT) / (ax | 1);
            p2.x += XY_ONE;
            ecount = (p2.x >> XY_SHIFT) - (p1.x >> XY_SHIFT);
            p1.y += ((y_step * -(p1.x & (XY_ONE - 1))) >> XY_SHIFT) + (XY_ONE >> 1);
            slope = ((y_step >> (XY_SHIFT - 5)) & 0x3f) ^ if y_step < 0 { 0x3f } else { 0 };
            start = (p1.x >> (XY_SHIFT - 7)) & 0x78;
            end = (p2.x >> (XY_SHIFT - 7)) & 0x78;
        } else {
            dx = (dx ^ i) - i;
            if i != 0 {
                std::mem::swap(&mut p1, &mut p2);
            }
            x_step = (dx << XY_SHIFT) / (ay | 1);
            y_step = XY_ONE;
            p2.y += XY_ONE;
            ecount = (p2.y >> XY_SHIFT) - (p1.y >> XY_SHIFT);
            p1.x += ((x_step * -(p1.y & (XY_ONE - 1))) >> XY_SHIFT) + (XY_ONE >> 1);
            slope = ((x_step >> (XY_SHIFT - 5)) & 0x3f) ^ if x_step < 0 { 0x3f } else { 0 };
            start = (p1.y >> (XY_SHIFT - 7)) & 0x78;
            end = (p2.y >> (XY_SHIFT - 7)) & 0x78;
        }
        let slope = if slope & 0x20 != 0 { 0x100 } else { SLOPE_CORR[slope as usize] };
        let t0 = slope << 7;
        let t1 = ((0x78 - start) | 4) * slope;
        let t2 = (end | 4) * slope;
        let mut ep = [0i64; 9];
        ep[8] = slope;
        ep[1] = ((((end - start) & 0x78 | 4) * slope) >> 8) & 0x1ff;
        ep[3] = ep[1];
        ep[2] = (t1 >> 8) & 0x1ff;
        ep[4] = ((((end - start + 0x80) | 4) * slope) >> 8) & 0x1ff;
        ep[5] = ((t1 + t0) >> 8) & 0x1ff;
        ep[6] = (t2 >> 8) & 0x1ff;
        ep[7] = ((t2 + t0) >> 8) & 0x1ff;
        let (width, height) = (self.w as i64, self.h as i64);
        let mut scount = 0i64;
        let corner = |n: i64| ((((n >= 2) as i64) + 1) & (n | 2)) as usize;
        while ecount >= 0 {
            let ep_corr = ep[corner(scount) * 3 + corner(ecount)] as i32;
            let weight = |d: usize| ((ep_corr * FILTER[d]) >> 8) & 0xff;
            if along_x {
                let x = p1.x >> XY_SHIFT;
                if (0..width).contains(&x) {
                    let y = (p1.y >> XY_SHIFT) - 1;
                    let dist = ((p1.y >> (XY_SHIFT - 5)) & 31) as usize;
                    for (row, d) in [(y, dist + 32), (y + 1, dist), (y + 2, 63 - dist)] {
                        if (0..height).contains(&row) {
                            self.blend(x, row, colour, weight(d));
                        }
                    }
                }
            } else {
                let y = p1.y >> XY_SHIFT;
                if (0..height).contains(&y) {
                    let x = (p1.x >> XY_SHIFT) - 1;
                    let dist = ((p1.x >> (XY_SHIFT - 5)) & 31) as usize;
                    for (col, d) in [(x, dist + 32), (x + 1, dist), (x + 2, 63 - dist)] {
                        if (0..width).contains(&col) {
                            self.blend(col, y, colour, weight(d));
                        }
                    }
                }
            }
            p1.x += x_step;
            p1.y += y_step;
            scount += 1;
            ecount -= 1;
        }
    }

    /// FillConvexPoly for anti-aliased drawing, the points in 16.16 fixed point.
    fn fill_convex(&mut self, v: &[Pt], colour: [u8; 4]) {
        let n = v.len();
        let delta = XY_ONE >> 1;
        let (mut xmin, mut xmax, mut ymin, mut ymax) = (v[0].x, v[0].x, v[0].y, v[0].y);
        let mut imin = 0;
        let mut p0 = v[n - 1];
        for (i, &p) in v.iter().enumerate() {
            if p.y < ymin {
                ymin = p.y;
                imin = i;
            }
            ymax = ymax.max(p.y);
            xmax = xmax.max(p.x);
            xmin = xmin.min(p.x);
            self.line_aa(p0, p, colour);
            p0 = p;
        }
        let (xmin, xmax, ymin, ymax) = ((xmin + delta) >> XY_SHIFT, (xmax + delta) >> XY_SHIFT, (ymin + delta) >> XY_SHIFT, (ymax + delta) >> XY_SHIFT);
        if n < 3 || xmax < 0 || ymax < 0 || xmin >= self.w as i64 || ymin >= self.h as i64 {
            return;
        }
        let ymax = ymax.min(self.h as i64 - 1);
        #[derive(Clone, Copy)]
        struct Edge {
            idx: usize,
            di: usize,
            x: i64,
            dx: i64,
            ye: i64,
        }
        let mut edge = [Edge { idx: imin, di: 1, x: -XY_ONE, dx: 0, ye: ymin }, Edge { idx: imin, di: n - 1, x: -XY_ONE, dx: 0, ye: ymin }];
        let mut edges = n as i64;
        let mut y = ymin;
        loop {
            if y < ymax || y == ymin {
                for e in edge.iter_mut() {
                    if y >= e.ye {
                        let mut idx0 = e.idx;
                        let mut idx = (idx0 + e.di) % n;
                        loop {
                            let more = edges > 0;
                            edges -= 1;
                            if !more {
                                break;
                            }
                            let ty = (v[idx].y + delta) >> XY_SHIFT;
                            if ty > y {
                                let (xs, xe) = (v[idx0].x, v[idx].x);
                                e.ye = ty;
                                e.dx = ((xe - xs) * 2 + (ty - y)) / (2 * (ty - y));
                                e.x = xs;
                                e.idx = idx;
                                break;
                            }
                            idx0 = idx;
                            idx = (idx + e.di) % n;
                        }
                    }
                }
            }
            if edges < 0 {
                break;
            }
            if y >= 0 {
                let (left, right) = if edge[0].x > edge[1].x { (1, 0) } else { (0, 1) };
                let (xx1, xx2) = ((edge[left].x + XY_ONE - 1) >> XY_SHIFT, edge[right].x >> XY_SHIFT);
                if xx2 >= 0 && xx1 < self.w as i64 {
                    for x in xx1.max(0)..=xx2.min(self.w as i64 - 1) {
                        self.put(x as i32, y as i32, colour);
                    }
                }
            }
            edge[0].x += edge[0].dx;
            edge[1].x += edge[1].dx;
            y += 1;
            if y > ymax {
                break;
            }
        }
    }

    /// ThickLine. `flags` asks for a round cap at the start (1) and the end (2); `shift` is how many fractional bits the points carry.
    fn thick_line(&mut self, mut p0: Pt, mut p1: Pt, colour: [u8; 4], thickness: i32, flags: u32, shift: i64) {
        let inside = |p: Pt| p.x >= 0 && p.y >= 0 && p.x < self.w as i64 && p.y < self.h as i64;
        if thickness > 1 && shift == 0 && !(inside(p0) && inside(p1)) {
            // Clipped to the picture and a margin as wide as the line, so a line far outside ends where the margin does
            let t = thickness as i64;
            let (mut a, mut b) = (Pt { x: p0.x + t, y: p0.y + t }, Pt { x: p1.x + t, y: p1.y + t });
            if !clip_line((self.w as i64 + 2 * t, self.h as i64 + 2 * t), &mut a, &mut b) {
                return;
            }
            p0 = Pt { x: a.x - t, y: a.y - t };
            p1 = Pt { x: b.x - t, y: b.y - t };
        }
        let scale = |p: Pt| Pt { x: p.x << (XY_SHIFT - shift), y: p.y << (XY_SHIFT - shift) };
        let (mut p0, p1) = (scale(p0), scale(p1));
        if thickness <= 1 {
            self.line_aa(p0, p1, colour);
            return;
        }
        let inverse = 1.0 / XY_ONE as f64;
        let (dx, dy) = ((p0.x - p1.x) as f64 * inverse, (p1.y - p0.y) as f64 * inverse);
        let r = dx * dx + dy * dy;
        let odd = (thickness & 1) as f64;
        let half = (thickness as i64) << (XY_SHIFT - 1);
        if r.abs() > f64::EPSILON {
            let r = (half as f64 + odd * XY_ONE as f64 * 0.5) / r.sqrt();
            let (ox, oy) = (cv_round(dy * r), cv_round(dx * r));
            let corners = [Pt { x: p0.x + ox, y: p0.y + oy }, Pt { x: p0.x - ox, y: p0.y - oy }, Pt { x: p1.x - ox, y: p1.y - oy }, Pt { x: p1.x + ox, y: p1.y + oy }];
            self.fill_convex(&corners, colour);
        }
        let mut flags = flags;
        for _ in 0..2 {
            if flags & 1 != 0 {
                self.ellipse(p0, half, colour, -1);
            }
            p0 = p1;
            flags >>= 1;
        }
    }

    fn poly_line(&mut self, v: &[Pt], colour: [u8; 4], thickness: i32) {
        let mut flags = 3;
        for pair in v.windows(2) {
            self.thick_line(pair[0], pair[1], colour, thickness, flags, XY_SHIFT);
            flags = 2;
        }
    }

    /// EllipseEx for a whole circle: a negative thickness fills it.
    fn ellipse(&mut self, centre: Pt, radius: i64, colour: [u8; 4], thickness: i32) {
        let size = (radius + (XY_ONE >> 1)) >> XY_SHIFT;
        let delta = if size < 3 {
            90
        } else if size < 10 {
            30
        } else if size < 15 {
            18
        } else {
            5
        };
        let mut v: Vec<Pt> = Vec::new();
        let mut angle = 0;
        while angle < 360 + delta {
            let a = angle.min(360);
            let (x, y) = (radius as f64 * sin_table(450 - a), radius as f64 * sin_table(a));
            let (fx, fy) = (centre.x as f64 + x, centre.y as f64 + y);
            let mut p = Pt { x: cv_round(fx / XY_ONE as f64) << XY_SHIFT, y: cv_round(fy / XY_ONE as f64) << XY_SHIFT };
            p.x += cv_round(fx - p.x as f64);
            p.y += cv_round(fy - p.y as f64);
            if v.last() != Some(&p) {
                v.push(p);
            }
            angle += delta;
        }
        if v.len() == 1 {
            v = vec![centre, centre];
        }
        if thickness >= 0 {
            self.poly_line(&v, colour, thickness);
        } else {
            self.fill_convex(&v, colour);
        }
    }
}

/// cv::clipLine, on a size in the same fixed point as the points.
fn clip_line(size: (i64, i64), p1: &mut Pt, p2: &mut Pt) -> bool {
    let (right, bottom) = (size.0 - 1, size.1 - 1);
    if size.0 <= 0 || size.1 <= 0 {
        return false;
    }
    let code = |p: &Pt| (p.x < 0) as i32 + (p.x > right) as i32 * 2 + (p.y < 0) as i32 * 4 + (p.y > bottom) as i32 * 8;
    let (mut c1, mut c2) = (code(p1), code(p2));
    if c1 & c2 == 0 && c1 | c2 != 0 {
        if c1 & 12 != 0 {
            let a = if c1 < 8 { 0 } else { bottom };
            p1.x += ((a - p1.y) as f64 * (p2.x - p1.x) as f64 / (p2.y - p1.y) as f64) as i64;
            p1.y = a;
            c1 = (p1.x < 0) as i32 + (p1.x > right) as i32 * 2;
        }
        if c2 & 12 != 0 {
            let a = if c2 < 8 { 0 } else { bottom };
            p2.x += ((a - p2.y) as f64 * (p2.x - p1.x) as f64 / (p2.y - p1.y) as f64) as i64;
            p2.y = a;
            c2 = (p2.x < 0) as i32 + (p2.x > right) as i32 * 2;
        }
        if c1 & c2 == 0 && c1 | c2 != 0 {
            if c1 != 0 {
                let a = if c1 == 1 { 0 } else { right };
                p1.y += ((a - p1.x) as f64 * (p2.y - p1.y) as f64 / (p2.x - p1.x) as f64) as i64;
                p1.x = a;
                c1 = 0;
            }
            if c2 != 0 {
                let a = if c2 == 1 { 0 } else { right };
                p2.y += ((a - p2.x) as f64 * (p2.y - p1.y) as f64 / (p2.x - p1.x) as f64) as i64;
                p2.x = a;
                c2 = 0;
            }
        }
    }
    c1 | c2 == 0
}

/// How far the Plain font sits below its baseline and above it, in its own units.
const PLAIN_BASE: i64 = 5;
const PLAIN_CAP: i64 = 4;

/// The strokes of the characters the ASCII effect draws, from OpenCV's Hershey table.
fn plain_glyph(c: char) -> Option<&'static str> {
    Some(match c {
        ' ' => "NV",
        '.' => "PURURVSVSURU",
        ':' => "PURPRQSQSPRP RURVSVSURU",
        '-' => "LXNRVR",
        '=' => "LXNPVP NTVT",
        '+' => "LXRNRV NRVR",
        '*' => "MWRORU OPUT UPOT",
        '#' => "MXQLQY TLTY OQVQ OTVT",
        '%' => "LYPMQNQOPPOPNONNOMPMSNUNWMNV USTTTUUVVVWUWTVSUS",
        '@' => "KYTPTSUTVTWSWQVOUNSMQMONNOMQMSNUOVQWSWUV TQSPQPPQPSQTSTTS",
        _ => return None,
    })
}

/// cv2.getFontScaleFromHeight for FONT_HERSHEY_PLAIN at thickness 1.
pub fn plain_scale_from_height(pixels: i32) -> f64 {
    (pixels as f64 - 1.0) / (PLAIN_BASE + PLAIN_CAP) as f64
}

/// The width cv2.getTextSize gives a text in FONT_HERSHEY_PLAIN at thickness 1.
pub fn plain_text_width(text: &str, scale: f64) -> i32 {
    let mut view_x = 0.0;
    for c in text.chars() {
        if let Some(g) = plain_glyph(c) {
            let b = g.as_bytes();
            view_x += (b[1] as f64 - b[0] as f64) * scale;
        }
    }
    cv_round(view_x + 1.0) as i32
}

#[cfg(test)]
#[path = "cv_draw_tests.rs"]
mod tests;
