//! Menu-bar (macOS) / notification-area (Windows) icon. Not built on Linux,
//! where tray support would add a GTK dependency to the binary.

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub enum TrayAction {
    Show,
    PauseAll,
    ResumeAll,
    Quit,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod imp {
    use super::TrayAction;
    use std::sync::mpsc::{Receiver, channel};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

    pub struct Tray {
        _icon: TrayIcon,
        pub actions: Receiver<TrayAction>,
    }

    pub fn create(wake: impl Fn() + Send + Sync + Clone + 'static) -> Option<Tray> {
        let show = MenuItem::new("Show Rustorrent", true, None);
        let pause = MenuItem::new("Pause All", true, None);
        let resume = MenuItem::new("Resume All", true, None);
        let quit = MenuItem::new("Quit Rustorrent", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &pause,
            &resume,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .ok()?;

        // macOS: monochrome "template" image that adapts to the menu bar.
        #[cfg(target_os = "macos")]
        let png = &include_bytes!("../../assets/tray-template.png")[..];
        #[cfg(target_os = "windows")]
        let png = &include_bytes!("../../assets/icon-64.png")[..];
        let icon_data = eframe::icon_data::from_png_bytes(png).ok()?;
        let icon = Icon::from_rgba(icon_data.rgba, icon_data.width, icon_data.height).ok()?;

        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Rustorrent");
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon);
        #[cfg(target_os = "windows")]
        let builder = builder.with_icon(icon);
        let tray = builder
            .build()
            .map_err(|e| tracing::warn!("tray icon unavailable: {e}"))
            .ok()?;

        let (tx, rx) = channel();
        let ids = [
            (show.id().clone(), TrayAction::Show),
            (pause.id().clone(), TrayAction::PauseAll),
            (resume.id().clone(), TrayAction::ResumeAll),
            (quit.id().clone(), TrayAction::Quit),
        ];
        let menu_tx = tx.clone();
        let menu_wake = wake.clone();
        let handler: tray_icon::menu::MenuEventHandler =
            std::sync::Arc::new(move |e: MenuEvent| {
                if let Some((_, a)) = ids.iter().find(|(id, _)| *id == e.id) {
                    let _ = menu_tx.send(*a);
                    menu_wake();
                }
            });
        MenuEvent::set_event_handler(Some(handler));
        // Windows: left-click on the icon shows the window.
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            } = e
            {
                let _ = tx.send(TrayAction::Show);
                wake();
            }
        }));
        Some(Tray {
            _icon: tray,
            actions: rx,
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    use super::TrayAction;
    pub struct Tray {
        pub actions: std::sync::mpsc::Receiver<TrayAction>,
    }
    pub fn create(_wake: impl Fn() + Send + Sync + Clone + 'static) -> Option<Tray> {
        None
    }
}

pub use imp::{Tray, create};

/// Whether this platform shows a tray / menu-bar icon.
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "windows"));
