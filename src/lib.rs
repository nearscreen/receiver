//! Nearscreen receiver — shows an iPhone screen streamed over the local network.
//!
//! The phone connects to us; we never reach out. The pieces:
//!
//! - [`net`] — the TCP server and the wire protocol (PROTOCOL.md).
//! - [`decode`] — hardware video decoding through the operating system.
//! - [`ui`] — the window, the tray icon and the consent dialog.
//! - [`config`] — the settings file.
//! - [`consent`] — who is allowed to stream here.

pub mod config;
pub mod consent;
pub mod decode;
pub mod net;
pub mod ui;
