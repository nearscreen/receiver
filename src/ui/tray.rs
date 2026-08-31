//! The tray icon and its menu — how the receiver is reached once its window is
//! out of the way.

use anyhow::{Context, Result};
use log::debug;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Icons are small; the mark is drawn into this square.
const ICON_SIZE: u32 = 32;

/// What the person picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    ShowHide,
    Borderless,
    StartAtLogin,
    Quit,
}

/// The tray icon, alive for as long as the receiver is.
pub struct Tray {
    icon: TrayIcon,
    show_hide: MenuItem,
    borderless: CheckMenuItem,
    start_at_login: CheckMenuItem,
}

impl Tray {
    /// Puts the icon in the tray. `chosen` is called on the thread the menu
    /// event arrives on, so it should do nothing but pass the choice along.
    pub fn new(
        start_at_login: bool,
        chosen: impl Fn(Choice) + Send + Sync + 'static,
    ) -> Result<Self> {
        let menu = Menu::new();
        let show_hide = MenuItem::new("Hide window", true, None);
        let borderless = CheckMenuItem::new("Borderless", true, false, None);
        let start_item = CheckMenuItem::new("Start at login", true, start_at_login, None);
        let quit = MenuItem::new("Quit", true, None);
        menu.append_items(&[
            &show_hide,
            &borderless,
            &start_item,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .context("cannot build the tray menu")?;

        let ids = vec![
            (show_hide.id().clone(), Choice::ShowHide),
            (borderless.id().clone(), Choice::Borderless),
            (start_item.id().clone(), Choice::StartAtLogin),
            (quit.id().clone(), Choice::Quit),
        ];

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Nearscreen")
            .with_icon(draw_icon(false)?)
            .build()
            .context("cannot put an icon in the tray")?;

        let lookup = ids;
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, choice)) = lookup.iter().find(|(id, _)| *id == event.id) {
                debug!("tray: {choice:?}");
                chosen(*choice);
            }
        }));

        Ok(Self {
            icon,
            show_hide,
            borderless,
            start_at_login: start_item,
        })
    }

    /// A green dot on the icon while a phone is streaming.
    pub fn set_streaming(&self, streaming: bool) {
        if let Ok(icon) = draw_icon(streaming) {
            let _ = self.icon.set_icon(Some(icon));
        }
    }

    /// Keeps the menu honest about what the window is doing.
    pub fn set_window_shown(&self, shown: bool) {
        self.show_hide
            .set_text(if shown { "Hide window" } else { "Show window" });
    }

    pub fn set_borderless(&self, borderless: bool) {
        self.borderless.set_checked(borderless);
    }

    pub fn set_start_at_login(&self, enabled: bool) {
        self.start_at_login.set_checked(enabled);
    }
}

/// The mark, small, with a live dot when a phone is streaming.
fn draw_icon(streaming: bool) -> Result<Icon> {
    let rgba = super::icon_pixels(ICON_SIZE, streaming);
    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).context("cannot draw the tray icon")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_drawn_at_the_size_the_tray_expects() {
        // Building a tray needs a desktop, but the picture on it does not.
        assert!(draw_icon(false).is_ok());
        assert!(draw_icon(true).is_ok());
    }
}
