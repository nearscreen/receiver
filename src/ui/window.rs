//! The receiver's window.
//!
//! Drawing is deliberately plain: the newest picture is scaled to fit and
//! centred on a dark background, with a title that does not change while a
//! phone comes and goes — a capture program keyed on the window title must
//! not lose it across a reconnect.

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use log::{debug, error, warn};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::decode::Nv12Frame;

/// The brand's dark ground, behind and around the picture.
const BACKGROUND: u32 = 0x060D10;

/// What the window is called before any phone has arrived.
const IDLE_TITLE: &str = "Nearscreen";

/// The newest decoded picture, replaced as fast as they come.
pub type FrameSlot = Arc<Mutex<Option<Nv12Frame>>>;

/// What the rest of the program tells the window.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// A new picture is in the slot.
    Frame,
    /// A phone is streaming; from now on the title names it.
    Streaming { device: String },
    /// The stream ended. The title stays as it was, on purpose.
    Idle,
}

/// Opens the window and runs until the person closes it.
///
/// `start` is handed the channel back into the window and is called before the
/// loop begins, so the caller can get its own machinery going.
pub fn run(frames: FrameSlot, start: impl FnOnce(EventLoopProxy<UiEvent>)) -> Result<()> {
    let event_loop = EventLoop::<UiEvent>::with_user_event()
        .build()
        .context("cannot open a window on this system")?;
    // Nothing animates on its own: we draw when a picture or a resize says so.
    event_loop.set_control_flow(ControlFlow::Wait);
    start(event_loop.create_proxy());

    let mut app = App {
        frames,
        title: IDLE_TITLE.to_string(),
        window: None,
        context: None,
        surface: None,
    };
    event_loop.run_app(&mut app).context("the window failed")?;
    Ok(())
}

struct App {
    frames: FrameSlot,
    title: String,
    window: Option<Arc<Window>>,
    context: Option<Context<Arc<Window>>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
}

impl App {
    fn draw(&mut self) {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
            return;
        };
        let size = window.inner_size();
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

        {
            let frame = self.frames.lock().unwrap_or_else(|e| e.into_inner());
            match frame.as_ref() {
                Some(frame) => frame.blit_fit(&mut buffer, size.width, size.height, BACKGROUND),
                None => buffer.fill(BACKGROUND),
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
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title(&self.title)
            // A phone screen is tall; start with something of that shape.
            .with_inner_size(LogicalSize::new(420.0, 880.0))
            .with_min_inner_size(LogicalSize::new(240.0, 240.0));
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
            UiEvent::Frame => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            UiEvent::Streaming { device } => {
                self.set_title(format!("{IDLE_TITLE} — {device}"));
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            UiEvent::Idle => {
                // The title is left alone so a capture source keeps its target.
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(_) => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}
