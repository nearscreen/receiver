//! The receiver's window: the phone's screen when one is streaming, and the
//! waiting screen when none is.
//!
//! The title does not change while a phone comes and goes — a capture program
//! keyed on the window title must not lose it across a reconnect.

use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use log::{debug, error, warn};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Fullscreen, Window, WindowId};

use super::paint::{colour, Canvas, Paint, Rect};
use super::text::{Style, Text, BOLD, REGULAR};
use super::waiting::Waiting;
use crate::decode::Nv12Frame;

/// What the window is called before any phone has arrived.
const IDLE_TITLE: &str = "Nearscreen";

/// Two clicks closer together than this are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// The newest decoded picture, replaced as fast as they come.
pub type FrameSlot = Arc<Mutex<Option<Nv12Frame>>>;

/// What the window needs to know about this computer.
pub struct WindowConfig {
    pub name: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
}

/// What the rest of the program tells the window.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// A new picture is in the slot.
    Frame,
    /// A phone is streaming; from now on the title names it.
    Streaming { device: String },
    /// How the stream is doing, for the overlay.
    Rate { summary: String },
    /// The stream ended. The title stays as it was, on purpose.
    Idle,
}

/// Opens the window and runs until the person closes it.
///
/// `start` is handed the way back into the window and is called before the
/// loop begins, so the caller can get its own machinery going.
pub fn run(
    config: WindowConfig,
    frames: FrameSlot,
    start: impl FnOnce(EventLoopProxy<UiEvent>),
) -> Result<()> {
    let event_loop = EventLoop::<UiEvent>::with_user_event()
        .build()
        .context("cannot open a window on this system")?;
    // Nothing animates on its own: we draw when a picture or a resize says so.
    event_loop.set_control_flow(ControlFlow::Wait);
    start(event_loop.create_proxy());

    let mut app = App {
        frames,
        text: Text::new(),
        waiting: Waiting::new(config.name, config.addresses, config.port),
        title: IDLE_TITLE.to_string(),
        device: None,
        rate: None,
        hovering: false,
        last_click: None,
        fullscreen: false,
        window: None,
        context: None,
        surface: None,
    };
    if app.text.is_none() {
        warn!("the bundled font is unreadable; the window will show no text");
    }
    event_loop.run_app(&mut app).context("the window failed")?;
    Ok(())
}

struct App {
    frames: FrameSlot,
    text: Option<Text>,
    waiting: Waiting,
    title: String,
    /// The phone currently streaming, for the overlay.
    device: Option<String>,
    /// "30 fps · 2.1 Mbit/s · H.264", refreshed while streaming.
    rate: Option<String>,
    hovering: bool,
    last_click: Option<Instant>,
    fullscreen: bool,
    window: Option<Arc<Window>>,
    context: Option<Context<Arc<Window>>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
}

impl App {
    fn redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn draw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return; // Minimised.
        };
        if let Err(e) = surface.resize(width, height) {
            warn!("cannot fit the window: {e}");
            return;
        }
        let mut buffer = match surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(e) => {
                warn!("cannot draw into the window: {e}");
                return;
            }
        };

        let picture = self.frames.lock().unwrap_or_else(|e| e.into_inner());
        match picture.as_ref() {
            Some(frame) => {
                frame.blit_fit(&mut buffer, size.width, size.height, colour::BACKGROUND);
                drop(picture);
                if self.hovering {
                    let mut canvas = Canvas::new(&mut buffer, size.width, size.height);
                    draw_overlay(
                        &mut canvas,
                        self.text.as_ref(),
                        self.device.as_deref(),
                        self.rate.as_deref(),
                        scale,
                    );
                }
            }
            None => {
                drop(picture);
                let mut canvas = Canvas::new(&mut buffer, size.width, size.height);
                match self.text.as_ref() {
                    Some(text) => self.waiting.draw(&mut canvas, text, scale),
                    None => canvas.clear(colour::BACKGROUND),
                }
            }
        }

        if let Err(e) = buffer.present() {
            warn!("cannot show the picture: {e}");
        }
    }

    fn set_title(&mut self, title: String) {
        if self.title == title {
            return;
        }
        self.title = title;
        if let Some(window) = &self.window {
            window.set_title(&self.title);
        }
    }

    fn toggle_fullscreen(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        self.fullscreen = !self.fullscreen;
        window.set_fullscreen(if self.fullscreen {
            Some(Fullscreen::Borderless(None))
        } else {
            None
        });
    }

    /// A click means different things on the two screens: while waiting it
    /// offers the next network address, while streaming it goes full screen.
    fn on_click(&mut self) {
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|previous| now.duration_since(previous) < DOUBLE_CLICK);
        self.last_click = Some(now);

        let streaming = self
            .frames
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        if streaming {
            if double {
                self.toggle_fullscreen();
                self.last_click = None;
            }
        } else {
            self.waiting.next_address();
            self.redraw();
        }
    }
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title(&self.title)
            // A phone screen is tall; start with something of that shape.
            .with_inner_size(LogicalSize::new(460.0, 900.0))
            .with_min_inner_size(LogicalSize::new(280.0, 320.0));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                error!("cannot open the window: {e}");
                event_loop.exit();
                return;
            }
        };
        let context = match Context::new(window.clone()) {
            Ok(context) => context,
            Err(e) => {
                error!("cannot prepare the window for drawing: {e}");
                event_loop.exit();
                return;
            }
        };
        match Surface::new(&context, window.clone()) {
            Ok(surface) => self.surface = Some(surface),
            Err(e) => {
                error!("cannot prepare the window for drawing: {e}");
                event_loop.exit();
                return;
            }
        }
        self.context = Some(context);
        self.window = Some(window);
        debug!("window ready");
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UiEvent) {
        match event {
            UiEvent::Frame => {}
            UiEvent::Streaming { device } => {
                self.set_title(format!("{IDLE_TITLE} — {device}"));
                self.device = Some(device);
                self.rate = None;
            }
            UiEvent::Rate { summary } => self.rate = Some(summary),
            UiEvent::Idle => {
                // The title is left alone so a capture source keeps its target.
                self.rate = None;
            }
        }
        self.redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(_) => self.redraw(),
            WindowEvent::CursorEntered { .. } => {
                self.hovering = true;
                self.redraw();
            }
            WindowEvent::CursorLeft { .. } => {
                self.hovering = false;
                self.redraw();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.on_click(),
            _ => {}
        }
    }
}

/// The thin strip over the picture: who is streaming and how well.
fn draw_overlay(
    canvas: &mut Canvas,
    text: Option<&Text>,
    device: Option<&str>,
    rate: Option<&str>,
    scale: f32,
) {
    let Some(text) = text else {
        return;
    };
    let s = scale;
    let device = device.unwrap_or("iPhone");
    let name = Style::new(13.0 * s, BOLD, Paint::Solid(colour::TEXT));
    let rate_style = Style::new(12.0 * s, REGULAR, Paint::Solid(colour::MUTED));
    let name_size = name.size;

    let widest = text
        .measure(device, &name)
        .max(rate.map_or(0.0, |rate| text.measure(rate, &rate_style)));
    let padding = 12.0 * s;
    let dot = 5.0 * s;
    let width = padding * 2.0 + dot * 2.0 + 8.0 * s + widest;
    let height = if rate.is_some() { 52.0 * s } else { 36.0 * s };
    let x = 16.0 * s;
    let y = canvas.height() - height - 16.0 * s;
    if width > canvas.width() - 32.0 * s || y < 0.0 {
        return; // Too small a window to put anything over the picture.
    }

    canvas.fill_round_rect(
        Rect::new(x, y, width, height),
        10.0 * s,
        Paint::Solid(colour::BAR),
    );
    canvas.stroke_round_rect(
        Rect::new(x, y, width, height),
        10.0 * s,
        1.0 * s,
        Paint::Solid(colour::BORDER),
    );

    let left = x + padding;
    let first = y + padding;
    canvas.fill_circle(
        left + dot,
        first + name_size * 0.55,
        dot,
        Paint::Solid(colour::LIVE),
    );
    text.draw(
        canvas,
        left + dot * 2.0 + 8.0 * s,
        first + text.ascent(name_size),
        device,
        &Style::new(name_size, BOLD, Paint::Solid(colour::TEXT)),
    );
    if let Some(rate) = rate {
        text.draw(
            canvas,
            left + dot * 2.0 + 8.0 * s,
            first + name_size * 1.45 + text.ascent(rate_style.size),
            rate,
            &rate_style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_overlay_stays_out_of_a_window_too_small_for_it() {
        let text = Text::new().unwrap();
        let mut pixels = vec![0u32; 40 * 40];
        let mut canvas = Canvas::new(&mut pixels, 40, 40);
        draw_overlay(
            &mut canvas,
            Some(&text),
            Some("iPhone (A1B2C3D4)"),
            Some("30 fps · 2.1 Mbit/s · H.264"),
            1.0,
        );
        assert!(pixels.iter().all(|p| *p == 0), "nothing should be drawn");
    }

    #[test]
    fn the_overlay_names_the_phone_over_the_picture() {
        let text = Text::new().unwrap();
        let mut pixels = vec![0xFFFFFFu32; 640 * 480];
        let mut canvas = Canvas::new(&mut pixels, 640, 480);
        draw_overlay(
            &mut canvas,
            Some(&text),
            Some("iPhone (A1B2C3D4)"),
            Some("30 fps · 2.1 Mbit/s · H.264"),
            1.0,
        );
        assert!(
            pixels.contains(&colour::LIVE),
            "the live dot should be there"
        );
        assert!(pixels.contains(&colour::BAR), "the strip should be there");
        // Only the bottom-left corner is covered; the picture stays.
        assert_eq!(
            pixels[0], 0xFFFFFF,
            "the top-left of the picture is untouched"
        );
    }
}
