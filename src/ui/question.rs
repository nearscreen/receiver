//! The question over the window: may this phone show its screen here?
//!
//! It sits on top of whatever the window was showing, dimmed, because there is
//! nothing else to do until it is answered.

use std::sync::mpsc::Sender;

use super::paint::{colour, Canvas, Paint, Rect};
use super::text::{Style, Text, BOLD, REGULAR};
use crate::consent::Answer;

const REASSURANCE: &str =
    "The stream stays on your local network. You can take this back at any time.";

/// One question, and the buttons the person can press.
pub struct Question {
    device: String,
    id: String,
    answer: Sender<(String, String, Answer)>,
    /// Filled in while drawing, so a click can be matched to a button.
    buttons: Vec<(Rect, Answer)>,
}

impl Question {
    pub fn new(device: String, id: String, answer: Sender<(String, String, Answer)>) -> Self {
        Self {
            device,
            id,
            answer,
            buttons: Vec::new(),
        }
    }

    /// The tail of the identifier, the way the phone shows it too.
    fn short_id(&self) -> String {
        let count = self.id.chars().count();
        self.id
            .chars()
            .skip(count.saturating_sub(8))
            .collect::<String>()
            .to_ascii_uppercase()
    }

    /// Tells whoever is waiting what the person decided.
    pub fn send(&self, answer: Answer) {
        let _ = self
            .answer
            .send((self.id.clone(), self.device.clone(), answer));
    }

    /// Which button, if any, is under this point.
    pub fn hit(&self, x: f32, y: f32) -> Option<Answer> {
        self.buttons
            .iter()
            .find(|(rect, _)| {
                x >= rect.x && x <= rect.x + rect.w && y >= rect.y && y <= rect.y + rect.h
            })
            .map(|(_, answer)| *answer)
    }

    pub fn draw(&mut self, canvas: &mut Canvas, text: &Text, scale: f32) {
        self.buttons.clear();
        canvas.dim(0.72);

        let s = scale;
        let (width, height) = (canvas.width(), canvas.height());
        let card_width = (420.0 * s).min(width - 32.0 * s);
        if card_width < 200.0 * s {
            return;
        }
        let padding = 24.0 * s;
        let inner = card_width - padding * 2.0;

        let title = Style::new(19.0 * s, BOLD, Paint::Solid(colour::TEXT));
        let sub = Style::new(13.0 * s, REGULAR, Paint::Solid(colour::MUTED));
        let body = Style::new(13.0 * s, REGULAR, Paint::Solid(colour::DIM));

        let headline = format!("{} wants to show its screen", self.device);
        let title_lines = text.wrap(&headline, &title, inner);
        let sub_line = format!("Device ID …{} · this network only", self.short_id());
        let body_lines = text.wrap(REASSURANCE, &body, inner);

        // Three buttons side by side unless the window is too narrow for them.
        let button_height = 38.0 * s;
        let gap = 8.0 * s;
        let side_by_side = inner >= 330.0 * s;
        let buttons_height = if side_by_side {
            button_height
        } else {
            button_height * 3.0 + gap * 2.0
        };

        let card_height = padding * 2.0
            + title_lines.len() as f32 * title.size * 1.3
            + 10.0 * s
            + sub.size * 1.4
            + 12.0 * s
            + body_lines.len() as f32 * body.size * 1.45
            + 20.0 * s
            + buttons_height;

        let x = (width - card_width) / 2.0;
        let y = ((height - card_height) / 2.0).max(16.0 * s);
        let card = Rect::new(x, y, card_width, card_height);
        canvas.fill_round_rect(card, 16.0 * s, Paint::Solid(colour::SURFACE));
        canvas.stroke_round_rect(card, 16.0 * s, 1.0 * s, Paint::Solid(colour::BORDER));

        let left = x + padding;
        let mut cursor = y + padding;
        for line in &title_lines {
            text.draw(canvas, left, cursor + text.ascent(title.size), line, &title);
            cursor += title.size * 1.3;
        }
        cursor += 10.0 * s;
        text.draw(
            canvas,
            left,
            cursor + text.ascent(sub.size),
            &sub_line,
            &sub,
        );
        cursor += sub.size * 1.4 + 12.0 * s;
        for line in &body_lines {
            text.draw(canvas, left, cursor + text.ascent(body.size), line, &body);
            cursor += body.size * 1.45;
        }
        cursor += 20.0 * s;

        let labels = [
            (Answer::Allow, "Allow", true),
            (Answer::Always, "Always allow", false),
            (Answer::Decline, "Decline", false),
        ];
        if side_by_side {
            let each = (inner - gap * 2.0) / 3.0;
            for (index, (answer, label, primary)) in labels.into_iter().enumerate() {
                let rect = Rect::new(
                    left + (each + gap) * index as f32,
                    cursor,
                    each,
                    button_height,
                );
                self.draw_button(canvas, text, rect, label, primary, s);
                self.buttons.push((rect, answer));
            }
        } else {
            for (index, (answer, label, primary)) in labels.into_iter().enumerate() {
                let rect = Rect::new(
                    left,
                    cursor + (button_height + gap) * index as f32,
                    inner,
                    button_height,
                );
                self.draw_button(canvas, text, rect, label, primary, s);
                self.buttons.push((rect, answer));
            }
        }
    }

    fn draw_button(
        &self,
        canvas: &mut Canvas,
        text: &Text,
        rect: Rect,
        label: &str,
        primary: bool,
        s: f32,
    ) {
        let radius = 10.0 * s;
        if primary {
            canvas.fill_round_rect(rect, radius, Paint::brand(rect.x, rect.w));
        } else {
            canvas.fill_round_rect(rect, radius, Paint::Solid(colour::BAR));
            canvas.stroke_round_rect(rect, radius, 1.0 * s, Paint::Solid(colour::BORDER));
        }
        let colour = if primary {
            colour::BACKGROUND
        } else {
            colour::TEXT
        };
        let style = Style::new(13.5 * s, BOLD, Paint::Solid(colour));
        text.draw_centred(
            canvas,
            rect.x + rect.w / 2.0,
            rect.y + rect.h / 2.0 + style.size * 0.36,
            label,
            &style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn question() -> (
        Question,
        std::sync::mpsc::Receiver<(String, String, Answer)>,
    ) {
        let (tx, rx) = channel();
        (
            Question::new(
                "iPhone “Ira”".to_string(),
                "VENDOR-ID-A1B2C3D4".to_string(),
                tx,
            ),
            rx,
        )
    }

    #[test]
    fn the_identifier_is_shortened_the_way_the_phone_shows_it() {
        let (question, _rx) = question();
        assert_eq!(question.short_id(), "A1B2C3D4");
    }

    #[test]
    fn every_button_can_be_pressed_and_says_what_it_means() {
        let text = Text::new().unwrap();
        let (mut question, answers) = question();
        let mut pixels = vec![0u32; 900 * 700];
        let mut canvas = Canvas::new(&mut pixels, 900, 700);
        question.draw(&mut canvas, &text, 1.0);

        assert_eq!(question.buttons.len(), 3, "three ways to answer");
        for (rect, expected) in question.buttons.clone() {
            let hit = question.hit(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            assert_eq!(hit, Some(expected));
        }
        assert_eq!(question.hit(1.0, 1.0), None, "the card is not a button");

        question.send(Answer::Always);
        let (id, device, answer) = answers.recv().unwrap();
        assert_eq!(id, "VENDOR-ID-A1B2C3D4");
        assert_eq!(device, "iPhone “Ira”");
        assert_eq!(answer, Answer::Always);
    }

    #[test]
    fn a_narrow_window_stacks_the_buttons_instead_of_dropping_them() {
        let text = Text::new().unwrap();
        let (mut question, _answers) = question();
        let mut pixels = vec![0u32; 320 * 700];
        let mut canvas = Canvas::new(&mut pixels, 320, 700);
        question.draw(&mut canvas, &text, 1.0);
        assert_eq!(question.buttons.len(), 3);
        let widths: Vec<f32> = question.buttons.iter().map(|(rect, _)| rect.w).collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "stacked buttons are the same width"
        );
    }
}
