use tokio::sync::mpsc::UnboundedSender;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug, Clone, Copy)]
pub enum TrayAction {
    ShowHide,
    PlayPause,
    Next,
    Quit,
}

pub struct SystemTray {
    _tray_icon: TrayIcon,
}

impl SystemTray {
    /// Create the Windows System Tray icon with Show/Hide, Play/Pause, Next, and Quit actions.
    pub fn new(action_tx: UnboundedSender<TrayAction>) -> Result<Self, String> {
        let menu = Menu::new();

        let show_hide_item = MenuItem::new("Show / Hide", true, None);
        let play_pause_item = MenuItem::new("Play / Pause", true, None);
        let next_item = MenuItem::new("Next Track", true, None);
        let separator = PredefinedMenuItem::separator();
        let quit_item = MenuItem::new("Quit", true, None);

        let show_hide_id = show_hide_item.id().clone();
        let play_pause_id = play_pause_item.id().clone();
        let next_id = next_item.id().clone();
        let quit_id = quit_item.id().clone();

        let _ = menu.append(&show_hide_item);
        let _ = menu.append(&play_pause_item);
        let _ = menu.append(&next_item);
        let _ = menu.append(&separator);
        let _ = menu.append(&quit_item);

        let icon = build_ytmusic_icon()?;

        let tray_icon = TrayIconBuilder::new()
            .with_tooltip("YouTube Music Native")
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .build()
            .map_err(|e| format!("Failed to build system tray icon: {}", e))?;

        std::thread::spawn(move || {
            let receiver = MenuEvent::receiver();
            while let Ok(event) = receiver.recv() {
                if event.id == show_hide_id {
                    let _ = action_tx.send(TrayAction::ShowHide);
                } else if event.id == play_pause_id {
                    let _ = action_tx.send(TrayAction::PlayPause);
                } else if event.id == next_id {
                    let _ = action_tx.send(TrayAction::Next);
                } else if event.id == quit_id {
                    let _ = action_tx.send(TrayAction::Quit);
                }
            }
        });

        Ok(Self {
            _tray_icon: tray_icon,
        })
    }
}

fn build_ytmusic_icon() -> Result<Icon, String> {
    let rgba = include_bytes!("tray_icon_rgba.bin").to_vec();
    Icon::from_rgba(rgba, 32, 32)
        .map_err(|e| format!("Failed to create tray RGBA icon: {}", e))
}
