//! Box drawing, block elements and Powerline arrows, drawn by us instead of
//! the font.
//!
//! Font glyphs for these rarely fill a cell exactly once the cell size is
//! rounded to whole pixels, so borders come out dashed and blocks misaligned.
//! Like Ghostty, we rasterize them to the exact cell size so neighbours join
//! seamlessly. The renderer caches the bitmaps alongside regular glyphs.

/// Whether `c` is drawn here rather than by the font.
pub fn is_sprite(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x259F | 0xE0B0..=0xE0B3)
}

/// A `width` x `height` coverage bitmap for `c`, or `None` if it isn't a sprite.
/// `thickness` is the width of a light line in pixels.
pub fn draw(c: char, width: u32, height: u32, thickness: u32) -> Option<Vec<u8>> {
    let mut canvas = Canvas::new(width, height, thickness.max(1));
    let cp = c as u32;
    match cp {
        0x2504..=0x250B | 0x254C..=0x254F => canvas.dashed(cp),
        0x2500..=0x256C | 0x2574..=0x257F => canvas.lines(ARMS[(cp - 0x2500) as usize]),
        0x256D..=0x2570 => canvas.arc(cp),
        0x2571..=0x2573 => canvas.diagonals(cp),
        0x2580..=0x259F => canvas.block(cp),
        0xE0B0..=0xE0B3 => canvas.powerline(cp),
        _ => return None,
    }
    Some(canvas.into_bytes())
}

/// Line weights for each box-drawing character from U+2500 to U+257F, as
/// left, right, up, down: 0 none, 1 light, 2 heavy, 3 double. Dashed lines,
/// arcs and diagonals (the zero rows in the middle) are drawn separately.
#[rustfmt::skip]
const ARMS: [[u8; 4]; 128] = [
    [1,1,0,0], [2,2,0,0], [0,0,1,1], [0,0,2,2], [0;4], [0;4], [0;4], [0;4],                 // 2500 ─━│┃ ┄┅┆┇
    [0;4], [0;4], [0;4], [0;4], [0,1,0,1], [0,2,0,1], [0,1,0,2], [0,2,0,2],                 // 2508 ┈┉┊┋ ┌┍┎┏
    [1,0,0,1], [2,0,0,1], [1,0,0,2], [2,0,0,2], [0,1,1,0], [0,2,1,0], [0,1,2,0], [0,2,2,0], // 2510 ┐┑┒┓ └┕┖┗
    [1,0,1,0], [2,0,1,0], [1,0,2,0], [2,0,2,0], [0,1,1,1], [0,2,1,1], [0,1,2,1], [0,1,1,2], // 2518 ┘┙┚┛ ├┝┞┟
    [0,1,2,2], [0,2,2,1], [0,2,1,2], [0,2,2,2], [1,0,1,1], [2,0,1,1], [1,0,2,1], [1,0,1,2], // 2520 ┠┡┢┣ ┤┥┦┧
    [1,0,2,2], [2,0,2,1], [2,0,1,2], [2,0,2,2], [1,1,0,1], [2,1,0,1], [1,2,0,1], [2,2,0,1], // 2528 ┨┩┪┫ ┬┭┮┯
    [1,1,0,2], [2,1,0,2], [1,2,0,2], [2,2,0,2], [1,1,1,0], [2,1,1,0], [1,2,1,0], [2,2,1,0], // 2530 ┰┱┲┳ ┴┵┶┷
    [1,1,2,0], [2,1,2,0], [1,2,2,0], [2,2,2,0], [1,1,1,1], [2,1,1,1], [1,2,1,1], [2,2,1,1], // 2538 ┸┹┺┻ ┼┽┾┿
    [1,1,2,1], [1,1,1,2], [1,1,2,2], [2,1,2,1], [1,2,2,1], [2,1,1,2], [1,2,1,2], [2,2,2,1], // 2540 ╀╁╂╃ ╄╅╆╇
    [2,2,1,2], [2,1,2,2], [1,2,2,2], [2,2,2,2], [0;4], [0;4], [0;4], [0;4],                 // 2548 ╈╉╊╋ ╌╍╎╏
    [3,3,0,0], [0,0,3,3], [0,3,0,1], [0,1,0,3], [0,3,0,3], [3,0,0,1], [1,0,0,3], [3,0,0,3], // 2550 ═║╒╓ ╔╕╖╗
    [0,3,1,0], [0,1,3,0], [0,3,3,0], [3,0,1,0], [1,0,3,0], [3,0,3,0], [0,3,1,1], [0,1,3,3], // 2558 ╘╙╚╛ ╜╝╞╟
    [0,3,3,3], [3,0,1,1], [1,0,3,3], [3,0,3,3], [3,3,0,1], [1,1,0,3], [3,3,0,3], [3,3,1,0], // 2560 ╠╡╢╣ ╤╥╦╧
    [1,1,3,0], [3,3,3,0], [3,3,1,1], [1,1,3,3], [3,3,3,3], [0;4], [0;4], [0;4],             // 2568 ╨╩╪╫ ╬╭╮╯
    [0;4], [0;4], [0;4], [0;4], [1,0,0,0], [0,0,1,0], [0,1,0,0], [0,0,0,1],                 // 2570 ╰╱╲╳ ╴╵╶╷
    [2,0,0,0], [0,0,2,0], [0,2,0,0], [0,0,0,2], [1,2,0,0], [0,0,1,2], [2,1,0,0], [0,0,2,1], // 2578 ╸╹╺╻ ╼╽╾╿
];

/// Floating-point coverage, one value per pixel, combined with `max`.
struct Canvas {
    width: u32,
    height: u32,
    /// Light line thickness in pixels.
    t: u32,
    pixels: Vec<f32>,
}

impl Canvas {
    fn new(width: u32, height: u32, t: u32) -> Canvas {
        Canvas { width, height, t, pixels: vec![0.0; (width * height) as usize] }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.pixels.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect()
    }

    /// Fills the pixels in [x0, x1) x [y0, y1), clipped to the canvas.
    fn rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, alpha: f32) {
        let (w, h) = (self.width as i32, self.height as i32);
        for y in y0.max(0)..y1.min(h) {
            for x in x0.max(0)..x1.min(w) {
                let p = &mut self.pixels[(y * w + x) as usize];
                *p = p.max(alpha);
            }
        }
    }

    /// Antialiased coverage from a per-pixel signed distance (negative inside).
    fn shade(&mut self, distance: impl Fn(f32, f32) -> f32) {
        for y in 0..self.height {
            for x in 0..self.width {
                let coverage = (0.5 - distance(x as f32 + 0.5, y as f32 + 0.5)).clamp(0.0, 1.0);
                let p = &mut self.pixels[(y * self.width + x) as usize];
                *p = p.max(coverage);
            }
        }
    }

    /// Where a line of `thickness` starts so it sits centered in `extent`.
    fn centered(extent: u32, thickness: u32) -> i32 {
        (extent.saturating_sub(thickness) / 2) as i32
    }

    fn weight(&self, w: u8) -> u32 {
        match w {
            2 => self.t * 2,
            _ => self.t,
        }
    }

    /// Draws arms from the cell's edges to its center.
    fn lines(&mut self, [left, right, up, down]: [u8; 4]) {
        let (w, h, t) = (self.width as i32, self.height as i32, self.t as i32);
        // Single (light or heavy) arms meet in a square as wide as the heaviest one.
        let heaviest = [left, right, up, down].iter().map(|&a| if a == 0 || a == 3 { 0 } else { self.weight(a) }).max();
        let core = heaviest.unwrap_or(0).max(self.t);
        let (cx, cy) = (Self::centered(self.width, core), Self::centered(self.height, core));
        let (cx1, cy1) = (cx + core as i32, cy + core as i32);
        for (arm, horizontal, towards_start) in
            [(left, true, true), (right, true, false), (up, false, true), (down, false, false)]
        {
            if arm == 0 || arm == 3 {
                continue;
            }
            let th = self.weight(arm);
            if horizontal {
                let y0 = Self::centered(self.height, th);
                let (x0, x1) = if towards_start { (0, cx1) } else { (cx, w) };
                self.rect(x0, y0, x1, y0 + th as i32, 1.0);
            } else {
                let x0 = Self::centered(self.width, th);
                let (y0, y1) = if towards_start { (0, cy1) } else { (cy, h) };
                self.rect(x0, y0, x0 + th as i32, y1, 1.0);
            }
        }

        // Double arms: two light lines a line-width apart on each side of the
        // center. Each line stops short where it would cross into the gap of
        // a perpendicular double line, and runs on to meet one otherwise.
        let has = |a: u8| a != 0;
        let (mx, my) = (Self::centered(self.width, t as u32), Self::centered(self.height, t as u32));
        let (outer_x, inner_x) = (mx - t, mx + t); // left and right vertical lines
        let (outer_y, inner_y) = (my - t, my + t); // top and bottom horizontal lines
        for (y, side_open) in [(outer_y, has(up)), (inner_y, has(down))] {
            if left == 3 {
                let end = if side_open {
                    outer_x + t
                } else if has(right) {
                    w
                } else {
                    inner_x + t
                };
                self.rect(0, y, end, y + t, 1.0);
            }
            if right == 3 {
                let start = if side_open {
                    inner_x
                } else if has(left) {
                    0
                } else {
                    outer_x
                };
                self.rect(start, y, w, y + t, 1.0);
            }
        }
        for (x, side_open) in [(outer_x, has(left)), (inner_x, has(right))] {
            if up == 3 {
                let end = if side_open {
                    outer_y + t
                } else if has(down) {
                    h
                } else {
                    inner_y + t
                };
                self.rect(x, 0, x + t, end, 1.0);
            }
            if down == 3 {
                let start = if side_open {
                    inner_y
                } else if has(up) {
                    0
                } else {
                    outer_y
                };
                self.rect(x, start, x + t, h, 1.0);
            }
        }
    }

    /// ┄┅┆┇┈┉┊┋╌╍╎╏: a line broken into 2, 3 or 4 dashes.
    fn dashed(&mut self, cp: u32) {
        let (dashes, heavy, vertical) = match cp {
            0x2504..=0x2507 => (3, cp & 1 == 1, cp >= 0x2506),
            0x2508..=0x250B => (4, cp & 1 == 1, cp >= 0x250A),
            _ => (2, cp & 1 == 1, cp >= 0x254E),
        };
        let th = if heavy { self.t * 2 } else { self.t };
        let extent = if vertical { self.height } else { self.width } as f32;
        let segment = extent / dashes as f32;
        for i in 0..dashes {
            let a = (i as f32 * segment + segment * 0.2).round() as i32;
            let b = (i as f32 * segment + segment * 0.8).round() as i32;
            if vertical {
                let x = Self::centered(self.width, th);
                self.rect(x, a, x + th as i32, b, 1.0);
            } else {
                let y = Self::centered(self.height, th);
                self.rect(a, y, b, y + th as i32, 1.0);
            }
        }
    }

    /// ╭╮╯╰: a quarter circle joining two arms, the way Claude Code draws boxes.
    fn arc(&mut self, cp: u32) {
        let t = self.t as f32;
        // The centerline the arms would follow, at the middle of a light line.
        let mx = Self::centered(self.width, self.t) as f32 + t / 2.0;
        let my = Self::centered(self.height, self.t) as f32 + t / 2.0;
        let r = mx.min(my);
        let (right, down) = match cp {
            0x256D => (true, true),   // ╭
            0x256E => (false, true),  // ╮
            0x256F => (false, false), // ╯
            _ => (true, false),       // ╰
        };
        let (w, h) = (self.width as f32, self.height as f32);
        // The circle's center sits diagonally off the corner, towards the arms.
        let cx = if right { mx + r } else { mx - r };
        let cy = if down { my + r } else { my - r };
        // Straight remainders of the arms beyond the arc.
        let (hx0, hx1) = if right { (cx, w) } else { (0.0, cx) };
        let (vy0, vy1) = if down { (cy, h) } else { (0.0, cy) };
        self.shade(|x, y| {
            let in_arc_quadrant = (if right { x <= cx } else { x >= cx }) && (if down { y <= cy } else { y >= cy });
            let ring = if in_arc_quadrant { ((x - cx).hypot(y - cy) - r).abs() - t / 2.0 } else { f32::MAX };
            let horizontal = if x >= hx0 && x <= hx1 { (y - my).abs() - t / 2.0 } else { f32::MAX };
            let vertical = if y >= vy0 && y <= vy1 { (x - mx).abs() - t / 2.0 } else { f32::MAX };
            ring.min(horizontal).min(vertical)
        });
    }

    /// ╱╲╳: lines from corner to corner.
    fn diagonals(&mut self, cp: u32) {
        let (w, h, t) = (self.width as f32, self.height as f32, self.t as f32);
        let len = w.hypot(h);
        // Distance from a point to the line through two corners.
        let rising = move |x: f32, y: f32| ((h - y) * w - x * h).abs() / len - t / 2.0; // ╱
        let falling = move |x: f32, y: f32| (y * w - x * h).abs() / len - t / 2.0; // ╲
        match cp {
            0x2571 => self.shade(rising),
            0x2572 => self.shade(falling),
            _ => self.shade(|x, y| rising(x, y).min(falling(x, y))),
        }
    }

    /// ▀▁▂▃▄▅▆▇█▉▊▋▌▍▎▏▐░▒▓▔▕ and the quadrants.
    fn block(&mut self, cp: u32) {
        let (w, h) = (self.width as i32, self.height as i32);
        let frac_h = |eighths: i32| (h * eighths + 4) / 8;
        let frac_w = |eighths: i32| (w * eighths + 4) / 8;
        let (half_w, half_h) = (frac_w(4), frac_h(4));
        match cp {
            0x2580 => self.rect(0, 0, w, half_h, 1.0),
            0x2581..=0x2588 => self.rect(0, h - frac_h((cp - 0x2580) as i32), w, h, 1.0),
            0x2589..=0x258F => self.rect(0, 0, frac_w((0x2590 - cp) as i32), h, 1.0),
            0x2590 => self.rect(half_w, 0, w, h, 1.0),
            0x2591..=0x2593 => self.rect(0, 0, w, h, (cp - 0x2590) as f32 * 0.25),
            0x2594 => self.rect(0, 0, w, frac_h(1), 1.0),
            0x2595 => self.rect(w - frac_w(1), 0, w, h, 1.0),
            _ => {
                // Quadrants: upper left, upper right, lower left, lower right.
                let quads: [bool; 4] = match cp {
                    0x2596 => [false, false, true, false],
                    0x2597 => [false, false, false, true],
                    0x2598 => [true, false, false, false],
                    0x2599 => [true, false, true, true],
                    0x259A => [true, false, false, true],
                    0x259B => [true, true, true, false],
                    0x259C => [true, true, false, true],
                    0x259D => [false, true, false, false],
                    0x259E => [false, true, true, false],
                    _ => [false, true, true, true],
                };
                let areas =
                    [(0, 0, half_w, half_h), (half_w, 0, w, half_h), (0, half_h, half_w, h), (half_w, half_h, w, h)];
                for (on, (x0, y0, x1, y1)) in quads.into_iter().zip(areas) {
                    if on {
                        self.rect(x0, y0, x1, y1, 1.0);
                    }
                }
            }
        }
    }

    /// Powerline separators: solid triangles and thin chevrons.
    fn powerline(&mut self, cp: u32) {
        let (w, h, t) = (self.width as f32, self.height as f32, self.t as f32);
        let points_right = cp <= 0xE0B1;
        let solid = cp.is_multiple_of(2);
        let tip = move |x: f32| if points_right { x } else { w - x };
        // The triangle edges run from the corners on the flat side to the tip
        // at mid-height; distance is measured along x, which is close enough
        // for lines this steep.
        let edge = move |x: f32, y: f32| {
            (tip(x) - (h / 2.0 - (y - h / 2.0).abs()) * (w / (h / 2.0))) * (h / 2.0) / (h / 2.0).hypot(w)
        };
        if solid {
            self.shade(edge);
        } else {
            self.shade(move |x, y| edge(x, y).abs() - t / 2.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(c: char) -> Vec<u8> {
        draw(c, 10, 20, 1).unwrap()
    }

    fn at(b: &[u8], x: usize, y: usize) -> u8 {
        b[y * 10 + x]
    }

    #[test]
    fn horizontal_lines_reach_both_edges() {
        let b = bitmap('─');
        let row = (0..20).find(|&y| at(&b, 5, y) == 255).expect("a solid row");
        assert!((0..10).all(|x| at(&b, x, row) == 255), "the line must touch both cell edges");
        assert_eq!(b.iter().filter(|&&v| v > 0).count(), 10, "one pixel thick");
    }

    #[test]
    fn vertical_lines_reach_top_and_bottom() {
        let b = bitmap('│');
        let col = (0..10).find(|&x| at(&b, x, 10) == 255).expect("a solid column");
        assert!((0..20).all(|y| at(&b, col, y) == 255));
    }

    #[test]
    fn corners_connect_their_arms() {
        let b = bitmap('┌');
        assert_eq!(at(&b, 9, 9).max(at(&b, 9, 10)), 255, "right arm reaches the right edge");
        assert_eq!(at(&b, 4, 19).max(at(&b, 5, 19)), 255, "down arm reaches the bottom");
        assert_eq!(at(&b, 0, 9).max(at(&b, 0, 10)), 0, "no left arm");
    }

    #[test]
    fn rounded_corners_meet_straight_neighbours() {
        // ╭ must line up with the ─ to its right and the │ below it.
        let arc = bitmap('╭');
        let h = bitmap('─');
        let v = bitmap('│');
        let h_row = (0..20).find(|&y| at(&h, 5, y) == 255).unwrap();
        let v_col = (0..10).find(|&x| at(&v, x, 10) == 255).unwrap();
        assert!(at(&arc, 9, h_row) > 200, "arc ends on the horizontal line's row");
        assert!(at(&arc, v_col, 19) > 200, "arc ends on the vertical line's column");
        assert_eq!(at(&arc, 0, 0), 0);
    }

    #[test]
    fn blocks_fill_exact_fractions() {
        assert!(bitmap('█').iter().all(|&v| v == 255));
        let upper = bitmap('▀');
        assert!((0..10).all(|y| at(&upper, 3, y) == 255) && (10..20).all(|y| at(&upper, 3, y) == 0));
        let shade = bitmap('▒');
        assert!(shade.iter().all(|&v| v == 128));
    }

    #[test]
    fn double_lines_are_two_lines() {
        let b = bitmap('═');
        let rows: Vec<usize> = (0..20).filter(|&y| at(&b, 5, y) == 255).collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[1] - rows[0] >= 2, "a gap between the two lines");
    }

    #[test]
    fn every_box_character_draws_something() {
        for cp in (0x2500..=0x259F).chain(0xE0B0..=0xE0B3) {
            let c = char::from_u32(cp).unwrap();
            assert!(is_sprite(c));
            let b = draw(c, 10, 20, 1).unwrap();
            assert!(b.iter().any(|&v| v > 0), "U+{cp:04X} drew nothing");
        }
        assert!(!is_sprite('a'));
    }
}
