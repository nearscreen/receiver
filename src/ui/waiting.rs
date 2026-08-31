//! The screen before a phone arrives: who this computer is, where it is on the
//! network, and a QR code that sets a phone up without anyone typing an
//! address — which is what saves the day on a network that blocks discovery.

use std::net::IpAddr;

use qrcode::{Color, QrCode};

use super::logo;
use super::paint::{colour, Canvas, Paint, Rect};
use super::text::{Style, Text, BOLD, REGULAR};

const HEADING: &str = "Waiting for your iPhone…";
const SUBTITLE: &str =
    "Open Nearscreen on your iPhone — this computer will appear in the list automatically.";
const QR_CAPTION: &str =
    "No auto-discovery on this network? Point your iPhone camera here — the app opens already connected.";
const NO_ADDRESS: &str = "no network address yet";

/// What the waiting screen knows.
pub struct Waiting {
    name: String,
    addresses: Vec<IpAddr>,
    port: u16,
    /// Which address the QR code points at, when there are several.
    chosen: usize,
    code: Option<Code>,
}

struct Code {
    url: String,
    size: usize,
    dark: Vec<bool>,
}

impl Waiting {
    pub fn new(name: String, addresses: Vec<IpAddr>, port: u16) -> Self {
        Self {
            name,
            addresses,
            port,
            chosen: 0,
            code: None,
        }
    }

    pub fn address(&self) -> Option<IpAddr> {
        self.addresses.get(self.chosen).copied()
    }

    /// Moves to the next address; a computer on several networks needs the one
    /// the phone can actually reach.
    pub fn next_address(&mut self) {
        if self.addresses.len() > 1 {
            self.chosen = (self.chosen + 1) % self.addresses.len();
            self.code = None;
        }
    }

    /// The link the QR code carries: opening it on the phone points the app
    /// straight at this receiver.
    pub fn url(&self) -> Option<String> {
        let address = self.address()?;
        Some(format!(
            "nearscreen://broadcast?host={address}&port={}",
            self.port
        ))
    }

    fn ensure_code(&mut self) {
        let Some(url) = self.url() else {
            self.code = None;
            return;
        };
        if self.code.as_ref().is_some_and(|code| code.url == url) {
            return;
        }
        self.code = QrCode::new(url.as_bytes()).ok().map(|code| Code {
            url,
            size: code.width(),
            dark: code
                .to_colors()
                .into_iter()
                .map(|module| module == Color::Dark)
                .collect(),
        });
    }

    pub fn draw(&mut self, canvas: &mut Canvas, text: &Text, scale: f32) {
        self.ensure_code();
        canvas.clear(colour::BACKGROUND);

        let s = scale;
        let (width, height) = (canvas.width(), canvas.height());
        let margin = 32.0 * s;
        let column = (400.0 * s).min(width - margin * 2.0);
        if column < 120.0 * s {
            return; // Too small to say anything useful.
        }

        // Side by side when there is room, stacked when there is not.
        let gap = 48.0 * s;
        let side_by_side = width >= column + gap + 220.0 * s;
        let qr_side = if side_by_side {
            (168.0 * s).min(width - column - gap - margin * 2.0)
        } else {
            (168.0 * s).min(column).min(height * 0.32)
        };
        let show_code = self.code.is_some() && qr_side >= 90.0 * s;

        let text_height = self.left_height(text, s, column);
        let code_height = if show_code {
            self.code_height(text, s, qr_side, column.min(210.0 * s))
        } else {
            0.0
        };

        if side_by_side {
            let total = column + gap + qr_side;
            let left = ((width - total) / 2.0).max(margin);
            let top = ((height - text_height.max(code_height)) / 2.0).max(margin);
            self.draw_left(canvas, text, left, top, column, s);
            if show_code {
                let centre = left + column + gap + qr_side / 2.0;
                let code_top = ((height - code_height) / 2.0).max(margin);
                self.draw_code(
                    canvas,
                    text,
                    Rect::new(centre - qr_side / 2.0, code_top, qr_side, qr_side),
                    210.0 * s,
                    s,
                );
            }
        } else {
            let spacing = if show_code { 28.0 * s } else { 0.0 };
            let total = text_height + spacing + code_height;
            let top = ((height - total) / 2.0).max(margin);
            let left = ((width - column) / 2.0).max(margin);
            self.draw_left(canvas, text, left, top, column, s);
            if show_code {
                self.draw_code(
                    canvas,
                    text,
                    Rect::new(
                        (width - qr_side) / 2.0,
                        top + text_height + spacing,
                        qr_side,
                        qr_side,
                    ),
                    column.min(210.0 * s),
                    s,
                );
            }
        }
    }

    fn left_height(&self, text: &Text, s: f32, column: f32) -> f32 {
        let subtitle_lines = text
            .wrap(
                SUBTITLE,
                &Style::new(14.5 * s, REGULAR, Paint::Solid(colour::TEXT)),
                column,
            )
            .len() as f32;
        72.0 * s                                  // the mark
            + 16.0 * s
            + 27.0 * 1.2 * s                      // heading
            + 14.0 * s
            + subtitle_lines * 14.5 * 1.5 * s     // subtitle
            + 18.0 * s
            + 62.0 * s                            // the card
            + 14.0 * s
            + 13.0 * 1.4 * s // the App Store line
    }

    fn draw_left(&self, canvas: &mut Canvas, text: &Text, x: f32, y: f32, column: f32, s: f32) {
        let mut cursor = y;
        logo::draw(canvas, x, cursor, 72.0 * s);
        cursor += 72.0 * s + 16.0 * s;

        let heading_size = 27.0 * s;
        text.draw(
            canvas,
            x,
            cursor + text.ascent(heading_size),
            HEADING,
            &Style::new(heading_size, BOLD, Paint::Solid(colour::TEXT)),
        );
        cursor += heading_size * 1.2 + 14.0 * s;

        let body = 14.5 * s;
        for line in text.wrap(
            SUBTITLE,
            &Style::new(body, REGULAR, Paint::Solid(colour::TEXT)),
            column,
        ) {
            text.draw(
                canvas,
                x,
                cursor + text.ascent(body),
                &line,
                &Style::new(body, REGULAR, Paint::Solid(colour::MUTED)),
            );
            cursor += body * 1.5;
        }
        cursor += 18.0 * s;

        self.draw_card(canvas, text, x, cursor, column, s);
        cursor += 62.0 * s + 14.0 * s;

        let small = 13.0 * s;
        let lead = "Don't have the app? ";
        let advance = text.draw(
            canvas,
            x,
            cursor + text.ascent(small),
            lead,
            &Style::new(small, REGULAR, Paint::Solid(colour::DIM)),
        );
        text.draw(
            canvas,
            x + advance,
            cursor + text.ascent(small),
            "Get Nearscreen on the App Store",
            &Style::new(small, REGULAR, Paint::Solid(colour::ACCENT)),
        );
    }

    /// The card naming this computer and the address a phone can reach it on.
    fn draw_card(&self, canvas: &mut Canvas, text: &Text, x: f32, y: f32, width: f32, s: f32) {
        let height = 62.0 * s;
        canvas.fill_round_rect(
            Rect::new(x, y, width, height),
            12.0 * s,
            Paint::Solid(colour::SURFACE),
        );
        canvas.stroke_round_rect(
            Rect::new(x, y, width, height),
            12.0 * s,
            1.0 * s,
            Paint::Solid(colour::BORDER),
        );

        let icon = 22.0 * s;
        draw_monitor(canvas, x + 16.0 * s, y + (height - icon) / 2.0, icon);

        let left = x + 16.0 * s + icon + 12.0 * s;
        let name_size = 15.0 * s;
        let address_size = 12.5 * s;
        let block = name_size * 1.25 + address_size * 1.4;
        let mut cursor = y + (height - block) / 2.0;
        text.draw(
            canvas,
            left,
            cursor + text.ascent(name_size),
            &self.name,
            &Style::new(name_size, BOLD, Paint::Solid(colour::TEXT)),
        );
        cursor += name_size * 1.25;

        let address = match self.address() {
            Some(address) => format!("{address} : {}", self.port),
            None => NO_ADDRESS.to_string(),
        };
        text.draw(
            canvas,
            left,
            cursor + text.ascent(address_size),
            &address,
            &Style::new(address_size, REGULAR, Paint::Solid(colour::MUTED)),
        );

        // The dot that says the receiver is listening.
        canvas.fill_circle(
            x + width - 16.0 * s - 4.0 * s,
            y + height / 2.0,
            4.0 * s,
            Paint::Solid(colour::ACCENT),
        );
    }

    fn code_height(&self, text: &Text, s: f32, side: f32, caption_width: f32) -> f32 {
        let caption = 13.0 * s;
        let lines = text
            .wrap(
                QR_CAPTION,
                &Style::new(caption, BOLD, Paint::Solid(colour::TEXT)),
                caption_width,
            )
            .len() as f32;
        side + 28.0 * s + 14.0 * s + lines * caption * 1.45
    }

    /// `area` is where the code and its caption go: centred on the middle of
    /// the box, starting at its top.
    fn draw_code(&self, canvas: &mut Canvas, text: &Text, area: Rect, caption_width: f32, s: f32) {
        let Some(code) = &self.code else {
            return;
        };
        let (centre_x, y, side) = (area.x + area.w / 2.0, area.y, area.w);
        let padding = 14.0 * s;
        let box_side = side + padding * 2.0;
        let left = centre_x - box_side / 2.0;
        canvas.fill_round_rect(
            Rect::new(left, y, box_side, box_side),
            16.0 * s,
            Paint::Solid(colour::WHITE),
        );

        // Modules are drawn on a whole-pixel grid: a QR code with soft edges
        // is a QR code a camera has to work for.
        let module = (side / code.size as f32).max(1.0).floor();
        let drawn = module * code.size as f32;
        let origin_x = (left + (box_side - drawn) / 2.0).round();
        let origin_y = (y + (box_side - drawn) / 2.0).round();
        for row in 0..code.size {
            for column in 0..code.size {
                if code.dark[row * code.size + column] {
                    canvas.fill_rect(
                        Rect::new(
                            origin_x + column as f32 * module,
                            origin_y + row as f32 * module,
                            module,
                            module,
                        ),
                        Paint::Solid(colour::BAR),
                    );
                }
            }
        }

        let caption = 13.0 * s;
        let mut cursor = y + box_side + 14.0 * s;
        for line in text.wrap(
            QR_CAPTION,
            &Style::new(caption, BOLD, Paint::Solid(colour::TEXT)),
            caption_width,
        ) {
            text.draw_centred(
                canvas,
                centre_x,
                cursor + text.ascent(caption),
                &line,
                &Style::new(caption, BOLD, Paint::Solid(colour::MUTED)),
            );
            cursor += caption * 1.45;
        }
    }
}

/// The little screen-on-a-stand from the design.
fn draw_monitor(canvas: &mut Canvas, x: f32, y: f32, size: f32) {
    let unit = size / 24.0;
    let paint = Paint::Solid(colour::ACCENT);
    canvas.stroke_round_rect(
        Rect::new(x + 2.0 * unit, y + 4.0 * unit, 20.0 * unit, 13.0 * unit),
        2.0 * unit,
        2.0 * unit,
        paint,
    );
    canvas.stroke_path(
        &[
            (x + 8.0 * unit, y + 21.0 * unit),
            (x + 16.0 * unit, y + 21.0 * unit),
        ],
        2.0 * unit,
        paint,
    );
    canvas.stroke_path(
        &[
            (x + 12.0 * unit, y + 17.0 * unit),
            (x + 12.0 * unit, y + 21.0 * unit),
        ],
        2.0 * unit,
        paint,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting() -> Waiting {
        Waiting::new(
            "IRA-PC".to_string(),
            vec!["192.168.1.42".parse().unwrap(), "10.0.0.5".parse().unwrap()],
            9913,
        )
    }

    #[test]
    fn the_qr_code_carries_a_link_the_app_understands() {
        let waiting = waiting();
        assert_eq!(
            waiting.url().unwrap(),
            "nearscreen://broadcast?host=192.168.1.42&port=9913"
        );
    }

    #[test]
    fn a_computer_on_several_networks_can_offer_each_one() {
        let mut waiting = waiting();
        assert_eq!(waiting.address().unwrap().to_string(), "192.168.1.42");
        waiting.next_address();
        assert_eq!(waiting.address().unwrap().to_string(), "10.0.0.5");
        waiting.next_address();
        assert_eq!(waiting.address().unwrap().to_string(), "192.168.1.42");
    }

    #[test]
    fn a_computer_with_no_address_still_draws() {
        let mut waiting = Waiting::new("IRA-PC".to_string(), Vec::new(), 9913);
        assert!(waiting.url().is_none());
        let text = Text::new().unwrap();
        let mut pixels = vec![0u32; 800 * 600];
        let mut canvas = Canvas::new(&mut pixels, 800, 600);
        waiting.draw(&mut canvas, &text, 1.0);
        assert!(pixels.iter().any(|p| *p != colour::BACKGROUND));
    }

    #[test]
    fn the_waiting_screen_paints_something_at_both_shapes() {
        let text = Text::new().unwrap();
        for (w, h) in [(960u32, 600u32), (420, 880)] {
            let mut waiting = waiting();
            let mut pixels = vec![0u32; (w * h) as usize];
            let mut canvas = Canvas::new(&mut pixels, w, h);
            waiting.draw(&mut canvas, &text, 1.0);
            let white = pixels.iter().filter(|p| **p == colour::WHITE).count();
            assert!(white > 1000, "the QR code should be there at {w}x{h}");
            assert!(
                pixels.contains(&colour::TEXT),
                "the heading should be there at {w}x{h}"
            );
        }
    }
}
