//! The window: the phone's screen, and the waiting state before it arrives.
//!
//! The tray icon, the QR code and the consent dialog land here too; for now
//! this is the picture and a stable title, which is what a capture program
//! needs to hold on to us across reconnects.

mod logo;
mod paint;
mod question;
mod text;
mod tray;
mod waiting;
mod window;

pub use tray::Choice;

use paint::{colour, Canvas, Paint, Rect};

/// The application icon as RGBA pixels: the mark on the brand's dark tile,
/// with the tile's corners cut away. The window, the tray icon and the files
/// the packaging needs all come from here, so there is one thing to change.
pub fn icon_pixels(size: u32, streaming: bool) -> Vec<u8> {
    let side = size as f32;
    let inset = side * 0.06;
    let tile = Rect::new(inset, inset, side - inset * 2.0, side - inset * 2.0);
    let radius = tile.w * 0.22;

    let mut pixels = vec![colour::BACKGROUND; (size * size) as usize];
    {
        let mut canvas = Canvas::new(&mut pixels, size, size);
        let mark = tile.w * 0.72;
        logo::draw(
            &mut canvas,
            tile.x + (tile.w - mark) / 2.0,
            tile.y + (tile.h - mark) / 2.0,
            mark,
        );
        if streaming {
            canvas.fill_circle(
                tile.x + tile.w - side * 0.12,
                tile.y + tile.h - side * 0.12,
                side * 0.14,
                Paint::Solid(colour::ACCENT),
            );
        }
    }

    let mask = paint::round_rect_mask(size, size, tile, radius);
    pixels
        .iter()
        .zip(mask)
        .flat_map(|(pixel, alpha)| [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, alpha])
        .collect()
}
pub use window::{run, FrameSlot, UiEvent, WindowConfig};
