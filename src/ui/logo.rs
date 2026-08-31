//! The Nearscreen mark: two screen brackets, waves between them, a dot in the
//! middle. The shapes are the ones from the design, in its 512-unit space,
//! scaled to whatever size the window asks for.

use super::paint::{colour, Canvas, Paint};

/// The space the mark is drawn in.
const SPACE: f32 = 512.0;

/// Draws the mark with its top-left corner at `x`, `y`, `size` across.
pub fn draw(canvas: &mut Canvas, x: f32, y: f32, size: f32) {
    let unit = size / SPACE;
    let at = |px: f32, py: f32| (x + px * unit, y + py * unit);
    let paint = Paint::brand(x, size);

    // The two brackets, each a straight run into a rounded corner.
    for mirrored in [false, true] {
        let flip = |px: f32| if mirrored { SPACE - px } else { px };
        let mut path = vec![at(flip(208.0), 150.0), at(flip(128.0), 122.0)];
        path.extend(curve(
            at(flip(128.0), 122.0),
            at(flip(96.0), 112.0),
            at(flip(96.0), 146.0),
        ));
        path.push(at(flip(96.0), 366.0));
        path.extend(curve(
            at(flip(96.0), 366.0),
            at(flip(96.0), 400.0),
            at(flip(128.0), 390.0),
        ));
        path.push(at(flip(208.0), 362.0));
        canvas.stroke_path(&path, 34.0 * unit, paint);
    }

    // Two waves on each side of the dot, the outer one wider than the inner.
    for mirrored in [false, true] {
        let flip = |px: f32| if mirrored { SPACE - px } else { px };
        for (start_x, control_x, start_y, end_y) in
            [(283.5, 324.5, 216.7, 295.3), (312.6, 375.4, 188.6, 323.4)]
        {
            let wave = curve(
                at(flip(start_x), start_y),
                at(flip(control_x), 256.0),
                at(flip(start_x), end_y),
            );
            canvas.stroke_path(&wave, 30.0 * unit, paint);
        }
    }

    let (cx, cy) = at(256.0, 256.0);
    canvas.fill_circle(cx, cy, 18.0 * unit, Paint::Solid(colour::ACCENT));
}

fn curve(from: (f32, f32), control: (f32, f32), to: (f32, f32)) -> Vec<(f32, f32)> {
    super::paint::quadratic(from, control, to, 12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_stays_inside_the_box_it_was_given() {
        let (w, h) = (80u32, 80u32);
        let mut pixels = vec![0u32; (w * h) as usize];
        let mut canvas = Canvas::new(&mut pixels, w, h);
        draw(&mut canvas, 8.0, 8.0, 64.0);

        let painted: Vec<usize> = pixels
            .iter()
            .enumerate()
            .filter(|(_, p)| **p != 0)
            .map(|(i, _)| i)
            .collect();
        assert!(!painted.is_empty(), "the mark should be drawn");
        for index in painted {
            let (px, py) = ((index % w as usize) as f32, (index / w as usize) as f32);
            assert!(
                (7.0..=73.0).contains(&px) && (7.0..=73.0).contains(&py),
                "ink outside the box at {px},{py}"
            );
        }
    }
}
