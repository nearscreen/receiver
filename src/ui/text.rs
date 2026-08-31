//! Text, drawn with the font that travels inside the binary.
//!
//! Nunito is the family the rest of Nearscreen uses. One variable file covers
//! every weight, so a single font — and a single licence — ships with the
//! receiver, and headings simply ask for a heavier axis.

use std::cell::RefCell;

use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::Format;
use swash::{FontRef, NormalizedCoord};

use super::paint::{Canvas, Paint};

const FONT: &[u8] = include_bytes!("../../assets/Nunito[wght].ttf");

/// Body text.
pub const REGULAR: f32 = 600.0;
/// Headings and anything that has to be read at a glance.
pub const BOLD: f32 = 800.0;

/// Size, weight and colour of a run of text.
#[derive(Clone, Copy)]
pub struct Style {
    pub size: f32,
    pub weight: f32,
    pub paint: Paint,
}

impl Style {
    pub fn new(size: f32, weight: f32, paint: Paint) -> Self {
        Self {
            size,
            weight,
            paint,
        }
    }
}

pub struct Text {
    font: FontRef<'static>,
    context: RefCell<ScaleContext>,
}

impl Text {
    /// `None` only if the font inside the binary is somehow unreadable.
    pub fn new() -> Option<Self> {
        Some(Self {
            font: FontRef::from_index(FONT, 0)?,
            context: RefCell::new(ScaleContext::new()),
        })
    }

    fn coords(&self, weight: f32) -> Vec<NormalizedCoord> {
        self.font
            .variations()
            .normalized_coords([("wght", weight)])
            .collect()
    }

    /// How wide this text will be.
    pub fn measure(&self, text: &str, style: &Style) -> f32 {
        let coords = self.coords(style.weight);
        let metrics = self.font.glyph_metrics(&coords).scale(style.size);
        let charmap = self.font.charmap();
        text.chars()
            .map(|c| metrics.advance_width(charmap.map(c)))
            .sum()
    }

    /// How far the tallest letters reach above the baseline.
    pub fn ascent(&self, size: f32) -> f32 {
        self.font.metrics(&[]).scale(size).ascent
    }

    /// Draws the text with its left edge at `x` and its baseline at `baseline`,
    /// and returns how far the pen moved.
    pub fn draw(
        &self,
        canvas: &mut Canvas,
        x: f32,
        baseline: f32,
        text: &str,
        style: &Style,
    ) -> f32 {
        let coords = self.coords(style.weight);
        let metrics = self.font.glyph_metrics(&coords).scale(style.size);
        let charmap = self.font.charmap();

        let mut context = self.context.borrow_mut();
        let mut scaler = context
            .builder(self.font)
            .size(style.size)
            .hint(true)
            .normalized_coords(&coords)
            .build();

        let mut pen = 0.0;
        for character in text.chars() {
            let glyph = charmap.map(character);
            if let Some(image) = Render::new(&[Source::Outline])
                .format(Format::Alpha)
                .render(&mut scaler, glyph)
            {
                let left = (x + pen).round() as i32 + image.placement.left;
                let top = baseline.round() as i32 - image.placement.top;
                canvas.blend_mask(
                    left,
                    top,
                    image.placement.width as usize,
                    image.placement.height as usize,
                    &image.data,
                    style.paint,
                );
            }
            pen += metrics.advance_width(glyph);
        }
        pen
    }

    /// Draws the text centred on `centre_x`.
    pub fn draw_centred(
        &self,
        canvas: &mut Canvas,
        centre_x: f32,
        baseline: f32,
        text: &str,
        style: &Style,
    ) {
        let width = self.measure(text, style);
        self.draw(canvas, centre_x - width / 2.0, baseline, text, style);
    }

    /// Breaks the text into lines that fit `max_width`, at word boundaries.
    pub fn wrap(&self, text: &str, style: &Style, max_width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut line = String::new();
        for word in text.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && self.measure(&candidate, style) > max_width {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = candidate;
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(size: f32, weight: f32) -> Style {
        Style::new(size, weight, Paint::Solid(0xFFFFFF))
    }

    #[test]
    fn the_font_inside_the_binary_is_usable() {
        let text = Text::new().expect("the bundled font should load");
        assert!(text.measure("Nearscreen", &style(16.0, BOLD)) > 0.0);
        assert!(text.ascent(16.0) > 0.0);
    }

    #[test]
    fn heavier_text_is_wider() {
        let text = Text::new().unwrap();
        let light = text.measure("Waiting for your iPhone", &style(20.0, 400.0));
        let heavy = text.measure("Waiting for your iPhone", &style(20.0, 900.0));
        assert!(heavy > light, "the weight axis should do something");
    }

    #[test]
    fn wrapping_keeps_words_whole_and_within_the_width() {
        let text = Text::new().unwrap();
        let body = style(14.0, REGULAR);
        let sentence = "Open Nearscreen on your iPhone and this computer will appear in the list";
        let lines = text.wrap(sentence, &body, 160.0);
        assert!(lines.len() > 1, "a long sentence should wrap");
        for line in &lines {
            assert!(
                text.measure(line, &body) <= 160.0 || !line.contains(' '),
                "line too wide: {line:?}"
            );
        }
        assert_eq!(lines.join(" "), sentence, "no word may be lost or repeated");
    }

    #[test]
    fn drawing_puts_ink_on_the_canvas() {
        let text = Text::new().unwrap();
        let mut pixels = vec![0u32; 200 * 60];
        let mut canvas = Canvas::new(&mut pixels, 200, 60);
        let advance = text.draw(&mut canvas, 10.0, 40.0, "Nearscreen", &style(24.0, BOLD));
        assert!(advance > 0.0);
        assert!(
            pixels.iter().any(|p| *p != 0),
            "something should have been drawn"
        );
    }
}
