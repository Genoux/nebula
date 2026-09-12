//! Software rendering into an ARGB8888 shared-memory buffer.
//!
//! No toolkit and no GPU: the HUD is one rounded rectangle, two circles, five
//! bars and a line of text, and a toolkit would mean GTK or Qt living beside a
//! realtime audio daemon for the sake of those shapes.

use crate::look::Rgb;
use std::collections::HashMap;

/// Tried in order at startup. Loaded from the system rather than vendored: a
/// missing font costs the label, and the orb's colour still tells the state.
const FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/noto/NotoSans-Medium.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
];

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn inset(self, by: f32) -> Self {
        Self {
            x: self.x + by,
            y: self.y + by,
            w: self.w - 2.0 * by,
            h: self.h - 2.0 * by,
        }
    }
}

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height * 4],
        }
    }

    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }

    /// Source-over onto premultiplied BGRA, which is how `wl_shm` ARGB8888 is
    /// laid out in memory on a little-endian machine.
    pub fn blend(&mut self, x: usize, y: usize, color: Rgb, alpha: f32) {
        if x >= self.width || y >= self.height || alpha <= 0.0 {
            return;
        }
        let a = alpha.min(1.0);
        let keep = 1.0 - a;
        let mix = |src: f32, dst: u8| {
            (src * a * 255.0 + dst as f32 * keep)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        let i = (y * self.width + x) * 4;
        let px = &mut self.pixels[i..i + 4];
        px[0] = mix(color.2, px[0]);
        px[1] = mix(color.1, px[1]);
        px[2] = mix(color.0, px[2]);
        px[3] = mix(1.0, px[3]);
    }

    pub fn rounded_rect(&mut self, rect: Rect, radius: f32, color: Rgb, alpha: f32) {
        self.shape(rect, color, alpha, |x, y| {
            rounded_rect_coverage(rect, radius, x, y)
        });
    }

    pub fn ring(&mut self, rect: Rect, radius: f32, width: f32, color: Rgb, alpha: f32) {
        let inner = rect.inset(width);
        self.shape(rect, color, alpha, |x, y| {
            rounded_rect_coverage(rect, radius, x, y)
                - rounded_rect_coverage(inner, (radius - width).max(0.0), x, y)
        });
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Rgb, alpha: f32) {
        let bounds = Rect {
            x: cx - r - 1.0,
            y: cy - r - 1.0,
            w: 2.0 * r + 2.0,
            h: 2.0 * r + 2.0,
        };
        self.shape(bounds, color, alpha, |x, y| {
            (r + 0.5 - ((x - cx).powi(2) + (y - cy).powi(2)).sqrt()).clamp(0.0, 1.0)
        });
    }

    /// An 8-bit coverage mask, as fontdue produces for a glyph.
    fn mask(&mut self, x: i32, y: i32, width: usize, mask: &[u8], color: Rgb, alpha: f32) {
        for (row, line) in mask.chunks(width.max(1)).enumerate() {
            for (col, &c) in line.iter().enumerate() {
                let (px, py) = (x + col as i32, y + row as i32);
                if px >= 0 && py >= 0 && c > 0 {
                    self.blend(px as usize, py as usize, color, alpha * c as f32 / 255.0);
                }
            }
        }
    }

    fn shape(&mut self, bounds: Rect, color: Rgb, alpha: f32, coverage: impl Fn(f32, f32) -> f32) {
        let x0 = bounds.x.floor().max(0.0) as usize;
        let y0 = bounds.y.floor().max(0.0) as usize;
        let x1 = ((bounds.x + bounds.w).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((bounds.y + bounds.h).ceil().max(0.0) as usize).min(self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let c = coverage(x as f32 + 0.5, y as f32 + 0.5);
                if c > 0.0 {
                    self.blend(x, y, color, alpha * c);
                }
            }
        }
    }
}

/// Signed-distance coverage, so edges are antialiased without supersampling.
fn rounded_rect_coverage(r: Rect, radius: f32, px: f32, py: f32) -> f32 {
    let (hw, hh) = (r.w / 2.0, r.h / 2.0);
    let radius = radius.min(hw).min(hh).max(0.0);
    let qx = (px - (r.x + hw)).abs() - (hw - radius);
    let qy = (py - (r.y + hh)).abs() - (hh - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let inside = qx.max(qy).min(0.0);
    (0.5 - (outside + inside - radius)).clamp(0.0, 1.0)
}

pub struct Text {
    font: fontdue::Font,
    px: f32,
    glyphs: HashMap<char, (fontdue::Metrics, Vec<u8>)>,
}

impl Text {
    pub fn load(px: f32) -> Option<Self> {
        let font = FONT_CANDIDATES.iter().find_map(|path| {
            let bytes = std::fs::read(path).ok()?;
            fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).ok()
        })?;
        Some(Self {
            font,
            px,
            glyphs: HashMap::new(),
        })
    }

    /// Glyphs are cached per size, so a scale change on the output empties it.
    pub fn set_px(&mut self, px: f32) {
        if (px - self.px).abs() > f32::EPSILON {
            self.px = px;
            self.glyphs.clear();
        }
    }

    pub fn width(&self, text: &str) -> f32 {
        text.chars()
            .map(|c| self.font.metrics(c, self.px).advance_width)
            .sum()
    }

    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        x: f32,
        baseline: f32,
        color: Rgb,
        alpha: f32,
    ) {
        let mut pen = x;
        for ch in text.chars() {
            let (font, px) = (&self.font, self.px);
            let (metrics, mask) = self
                .glyphs
                .entry(ch)
                .or_insert_with(|| font.rasterize(ch, px));
            let gx = (pen + metrics.xmin as f32).round() as i32;
            let gy = (baseline - metrics.height as f32 - metrics.ymin as f32).round() as i32;
            canvas.mask(gx, gy, metrics.width, mask, color, alpha);
            pen += metrics.advance_width;
        }
    }
}

/// Shorten to fit `max`, ending in an ellipsis. Error messages are the only
/// labels long enough to need it.
pub fn elide(text: &str, max: f32, width: impl Fn(&str) -> f32) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    (0..chars.len())
        .rev()
        .map(|n| format!("{}…", chars[..n].iter().collect::<String>().trim_end()))
        .find(|candidate| width(candidate) <= max)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgb = Rgb(1.0, 1.0, 1.0);

    fn pixel(c: &Canvas, x: usize, y: usize) -> [u8; 4] {
        let i = (y * c.width + x) * 4;
        c.pixels[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn opaque_paint_replaces_and_transparent_paint_does_nothing() {
        let mut c = Canvas::new(2, 1);
        c.blend(0, 0, Rgb(1.0, 0.0, 0.0), 1.0);
        c.blend(1, 0, Rgb(1.0, 0.0, 0.0), 0.0);
        assert_eq!(pixel(&c, 0, 0), [0, 0, 255, 255], "BGRA, red in byte 2");
        assert_eq!(pixel(&c, 1, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn half_alpha_is_premultiplied() {
        let mut c = Canvas::new(1, 1);
        c.blend(0, 0, WHITE, 0.5);
        assert_eq!(pixel(&c, 0, 0), [128, 128, 128, 128]);
    }

    #[test]
    fn out_of_bounds_paint_is_ignored_rather_than_a_panic() {
        let mut c = Canvas::new(4, 4);
        c.blend(10, 10, WHITE, 1.0);
        c.circle(-20.0, -20.0, 5.0, WHITE, 1.0);
    }

    #[test]
    fn a_pill_is_filled_in_the_middle_and_empty_in_the_corner() {
        let mut c = Canvas::new(40, 20);
        c.rounded_rect(
            Rect {
                x: 0.0,
                y: 0.0,
                w: 40.0,
                h: 20.0,
            },
            10.0,
            WHITE,
            1.0,
        );
        assert_eq!(pixel(&c, 20, 10)[3], 255);
        assert_eq!(
            pixel(&c, 0, 0)[3],
            0,
            "the corner of a fully rounded pill is outside it"
        );
    }

    #[test]
    fn a_ring_leaves_its_middle_empty() {
        let mut c = Canvas::new(40, 20);
        c.ring(
            Rect {
                x: 0.0,
                y: 0.0,
                w: 40.0,
                h: 20.0,
            },
            10.0,
            1.0,
            WHITE,
            1.0,
        );
        assert_eq!(pixel(&c, 20, 10)[3], 0);
        assert!(pixel(&c, 20, 0)[3] > 100, "the top edge is on the ring");
    }

    #[test]
    fn elide_keeps_what_fits_and_shortens_what_does_not() {
        let width = |s: &str| s.chars().count() as f32;
        assert_eq!(elide("short", 10.0, width), "short");
        let cut = elide("a much longer message", 8.0, width);
        assert!(cut.ends_with('…'));
        assert!(cut.chars().count() <= 8, "{cut}");
    }
}
