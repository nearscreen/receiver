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
pub use window::{run, FrameSlot, UiEvent, WindowConfig};
