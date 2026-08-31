//! A very small painter for the window's pixel buffer.
//!
//! Shapes are drawn from distances rather than by a rasterizer library: for
//! every pixel we work out how far it is from the shape and use that as
//! coverage. It is a few lines per shape, it antialiases for free, and it
//! keeps the receiver free of a drawing dependency.

/// The palette from the design, so the receiver and the phone look alike.
pub mod colour {
    /// The ground everything sits on.
    pub const BACKGROUND: u32 = 0x060D10;
    /// Panels and cards.
    pub const SURFACE: u32 = 0x0F1D21;
    /// The bar along the top of the window.
    pub const BAR: u32 = 0x0A1114;
    /// Hairlines between areas.
    pub const BORDER: u32 = 0x1B3138;
    /// Ordinary text.
    pub const TEXT: u32 = 0xE9F4F2;
    /// Secondary text.
    pub const MUTED: u32 = 0x8AA0A3;
    /// Text that should barely be there.
    pub const DIM: u32 = 0x5A6E6C;
    /// The accent: a live receiver, a link, a highlight.
    pub const ACCENT: u32 = 0x3ED488;
    /// The brand gradient runs from this…
    pub const BRAND_BLUE: u32 = 0x2BB8F5;
    /// …to this.
    pub const BRAND_GREEN: u32 = 0x4EE07A;
    /// A live indicator.
    pub const LIVE: u32 = 0xFF5468;
    pub const WHITE: u32 = 0xFFFFFF;
}

/// A box on the canvas.
#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// How to colour what is being drawn.
#[derive(Clone, Copy)]
pub enum Paint {
    Solid(u32),
    /// The brand gradient, left to right across `from_x`..`to_x`.
    Horizontal {
        from: u32,
        to: u32,
        from_x: f32,
        to_x: f32,
    },
}

impl Paint {
    /// The brand gradient across a box.
    pub fn brand(x: f32, width: f32) -> Self {
        Paint::Horizontal {
            from: colour::BRAND_BLUE,
            to: colour::BRAND_GREEN,
            from_x: x,
            to_x: x + width,
        }
    }

    fn at(&self, x: f32) -> u32 {
        match *self {
            Paint::Solid(colour) => colour,
            Paint::Horizontal {
                from,
                to,
                from_x,
                to_x,
            } => {
                let span = to_x - from_x;
                let t = if span.abs() < f32::EPSILON {
                    0.0
                } else {
                    ((x - from_x) / span).clamp(0.0, 1.0)
                };
                mix(from, to, t)
            }
        }
    }
}

/// Blends two colours; `t` runs from `a` to `b`.
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |shift: u32| {
        let from = ((a >> shift) & 0xFF) as f32;
        let to = ((b >> shift) & 0xFF) as f32;
        ((from + (to - from) * t).round() as u32).min(255) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// The window's pixels, and everything we can put on them.
pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: i32,
    height: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32) -> Self {
        Self {
            pixels,
            width: width as i32,
            height: height as i32,
        }
    }

    pub fn width(&self) -> f32 {
        self.width as f32
    }

    pub fn height(&self) -> f32 {
        self.height as f32
    }

    pub fn clear(&mut self, colour: u32) {
        self.pixels.fill(colour);
    }

    /// Fades everything drawn so far towards the background, so that a
    /// question laid over it is the only thing worth looking at.
    pub fn dim(&mut self, amount: f32) {
        for pixel in self.pixels.iter_mut() {
            *pixel = mix(*pixel, colour::BACKGROUND, amount);
        }
    }

    /// Puts `colour` on the pixel, `coverage` deciding how much of it lands.
    pub fn blend(&mut self, x: i32, y: i32, colour: u32, coverage: f32) {
        if coverage <= 0.0 || x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let index = (y * self.width + x) as usize;
        let Some(pixel) = self.pixels.get_mut(index) else {
            return;
        };
        *pixel = if coverage >= 1.0 {
            colour
        } else {
            mix(*pixel, colour, coverage)
        };
    }

    /// Walks the pixels a shape can possibly touch.
    fn for_bounds(&self, x: f32, y: f32, w: f32, h: f32) -> (i32, i32, i32, i32) {
        let left = (x.floor() as i32).max(0);
        let top = (y.floor() as i32).max(0);
        let right = ((x + w).ceil() as i32).min(self.width);
        let bottom = ((y + h).ceil() as i32).min(self.height);
        (left, top, right, bottom)
    }

    pub fn fill_rect(&mut self, rect: Rect, paint: Paint) {
        self.fill_round_rect(rect, 0.0, paint);
    }

    /// A box with rounded corners, drawn from its distance field.
    pub fn fill_round_rect(&mut self, rect: Rect, radius: f32, paint: Paint) {
        let Rect { x, y, w, h } = rect;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let radius = radius.min(w / 2.0).min(h / 2.0).max(0.0);
        let (left, top, right, bottom) = self.for_bounds(x - 1.0, y - 1.0, w + 2.0, h + 2.0);
        for py in top..bottom {
            for px in left..right {
                let distance =
                    round_rect_distance(px as f32 + 0.5, py as f32 + 0.5, x, y, w, h, radius);
                let coverage = (0.5 - distance).clamp(0.0, 1.0);
                self.blend(px, py, paint.at(px as f32 + 0.5), coverage);
            }
        }
    }

    /// The hairline around a card.
    pub fn stroke_round_rect(&mut self, rect: Rect, radius: f32, thickness: f32, paint: Paint) {
        let Rect { x, y, w, h } = rect;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let radius = radius.min(w / 2.0).min(h / 2.0).max(0.0);
        let half = thickness / 2.0;
        let (left, top, right, bottom) = self.for_bounds(
            x - half - 1.0,
            y - half - 1.0,
            w + thickness + 2.0,
            h + thickness + 2.0,
        );
        for py in top..bottom {
            for px in left..right {
                let distance =
                    round_rect_distance(px as f32 + 0.5, py as f32 + 0.5, x, y, w, h, radius).abs();
                let coverage = (half + 0.5 - distance).clamp(0.0, 1.0);
                self.blend(px, py, paint.at(px as f32 + 0.5), coverage);
            }
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, paint: Paint) {
        let (left, top, right, bottom) = self.for_bounds(
            cx - radius - 1.0,
            cy - radius - 1.0,
            radius * 2.0 + 2.0,
            radius * 2.0 + 2.0,
        );
        for py in top..bottom {
            for px in left..right {
                let dx = px as f32 + 0.5 - cx;
                let dy = py as f32 + 0.5 - cy;
                let coverage = (0.5 - (dx.hypot(dy) - radius)).clamp(0.0, 1.0);
                self.blend(px, py, paint.at(px as f32 + 0.5), coverage);
            }
        }
    }

    /// A thick line through the points, with round ends and corners — the way
    /// the logo's brackets and waves are drawn.
    pub fn stroke_path(&mut self, points: &[(f32, f32)], thickness: f32, paint: Paint) {
        if points.len() < 2 {
            return;
        }
        let half = thickness / 2.0;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in points {
            min_x = min_x.min(*x);
            min_y = min_y.min(*y);
            max_x = max_x.max(*x);
            max_y = max_y.max(*y);
        }
        let (left, top, right, bottom) = self.for_bounds(
            min_x - half - 1.0,
            min_y - half - 1.0,
            (max_x - min_x) + thickness + 2.0,
            (max_y - min_y) + thickness + 2.0,
        );
        for py in top..bottom {
            for px in left..right {
                let point = (px as f32 + 0.5, py as f32 + 0.5);
                let mut distance = f32::MAX;
                for segment in points.windows(2) {
                    distance = distance.min(segment_distance(point, segment[0], segment[1]));
                }
                let coverage = (half + 0.5 - distance).clamp(0.0, 1.0);
                self.blend(px, py, paint.at(point.0), coverage);
            }
        }
    }

    /// Puts a rasterised glyph — one coverage byte per pixel — on the canvas.
    pub fn blend_mask(&mut self, x: i32, y: i32, w: usize, h: usize, mask: &[u8], paint: Paint) {
        for row in 0..h {
            for column in 0..w {
                let Some(coverage) = mask.get(row * w + column) else {
                    return;
                };
                if *coverage == 0 {
                    continue;
                }
                let px = x + column as i32;
                self.blend(
                    px,
                    y + row as i32,
                    paint.at(px as f32 + 0.5),
                    f32::from(*coverage) / 255.0,
                );
            }
        }
    }
}

/// Flattens a quadratic curve — the shape of the logo's waves — into points.
pub fn quadratic(
    from: (f32, f32),
    control: (f32, f32),
    to: (f32, f32),
    steps: usize,
) -> Vec<(f32, f32)> {
    (0..=steps)
        .map(|step| {
            let t = step as f32 / steps as f32;
            let u = 1.0 - t;
            (
                u * u * from.0 + 2.0 * u * t * control.0 + t * t * to.0,
                u * u * from.1 + 2.0 * u * t * control.1 + t * t * to.1,
            )
        })
        .collect()
}

/// Signed distance to a rounded box: negative inside, positive outside.
fn round_rect_distance(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, radius: f32) -> f32 {
    let half_w = w / 2.0;
    let half_h = h / 2.0;
    let dx = (px - (x + half_w)).abs() - (half_w - radius);
    let dy = (py - (y + half_h)).abs() - (half_h - radius);
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    outside + dx.max(dy).min(0.0) - radius
}

fn segment_distance(point: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (px, py) = (point.0 - a.0, point.1 - a.1);
    let (bx, by) = (b.0 - a.0, b.1 - a.1);
    let length = bx * bx + by * by;
    let t = if length <= f32::EPSILON {
        0.0
    } else {
        ((px * bx + py * by) / length).clamp(0.0, 1.0)
    };
    (px - bx * t).hypot(py - by * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(size: u32) -> (Vec<u32>, u32) {
        (vec![0u32; (size * size) as usize], size)
    }

    #[test]
    fn mixing_moves_from_one_colour_to_the_other() {
        assert_eq!(mix(0x000000, 0xFFFFFF, 0.0), 0x000000);
        assert_eq!(mix(0x000000, 0xFFFFFF, 1.0), 0xFFFFFF);
        assert_eq!(mix(0x000000, 0xFFFFFF, 0.5), 0x808080);
    }

    #[test]
    fn a_filled_box_covers_exactly_its_pixels() {
        let (mut pixels, size) = canvas(8);
        let mut canvas = Canvas::new(&mut pixels, size, size);
        canvas.fill_rect(Rect::new(2.0, 2.0, 4.0, 4.0), Paint::Solid(0xFFFFFF));
        let filled = pixels.iter().filter(|p| **p == 0xFFFFFF).count();
        assert_eq!(filled, 16);
        assert_eq!(pixels[0], 0x000000, "outside stays untouched");
    }

    #[test]
    fn rounded_corners_are_actually_rounded() {
        let (mut pixels, size) = canvas(16);
        let mut canvas = Canvas::new(&mut pixels, size, size);
        canvas.fill_round_rect(Rect::new(0.0, 0.0, 16.0, 16.0), 6.0, Paint::Solid(0xFFFFFF));
        assert_eq!(pixels[0], 0x000000, "the corner pixel is cut away");
        let middle = (8 * 16 + 8) as usize;
        assert_eq!(pixels[middle], 0xFFFFFF, "the middle is solid");
    }

    #[test]
    fn a_gradient_runs_the_way_it_is_told() {
        let (mut pixels, size) = canvas(8);
        let mut canvas = Canvas::new(&mut pixels, size, size);
        canvas.fill_rect(
            Rect::new(0.0, 0.0, 8.0, 8.0),
            Paint::Horizontal {
                from: 0x000000,
                to: 0xFFFFFF,
                from_x: 0.0,
                to_x: 8.0,
            },
        );
        assert!(pixels[0] < pixels[7], "left is darker than right");
    }

    #[test]
    fn a_stroke_lands_on_the_line_and_nowhere_else() {
        let (mut pixels, size) = canvas(16);
        let mut canvas = Canvas::new(&mut pixels, size, size);
        canvas.stroke_path(&[(2.0, 8.0), (14.0, 8.0)], 4.0, Paint::Solid(0xFFFFFF));
        assert_eq!(pixels[8 * 16 + 8], 0xFFFFFF, "on the line");
        assert_eq!(pixels[16 + 8], 0x000000, "far above it");
    }

    #[test]
    fn a_curve_starts_and_ends_where_it_should() {
        let points = quadratic((0.0, 0.0), (10.0, 0.0), (10.0, 10.0), 8);
        assert_eq!(points.first(), Some(&(0.0, 0.0)));
        assert_eq!(points.last(), Some(&(10.0, 10.0)));
        assert_eq!(points.len(), 9);
    }
}
