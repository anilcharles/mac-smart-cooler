//! Menu bar item: live status, the three modes with shortcuts, open window, quit.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use tray_icon::menu::{
    accelerator::Accelerator, CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Mode(usize), // 0 Apple Default, 1 Smart Cooling, 2 Full Blast
    Open,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    status: MenuItem,
    modes: [CheckMenuItem; 3],
    open: MenuItem,
    quit: MenuItem,
    rx: Receiver<tray_icon::menu::MenuId>,
    last_title: String,
}

/// 36x36 template image (black + alpha) of the fan glyph; macOS tints it for light/dark.
fn glyph() -> Icon {
    let n = 36usize;
    let mut px = vec![0u8; n * n * 4];
    let c = (n as f32 - 1.0) / 2.0;
    for y in 0..n {
        for x in 0..n {
            // 4x4 supersampling for smooth edges
            let mut cov = 0.0f32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let dx = x as f32 + (sx as f32 + 0.5) / 4.0 - 0.5 - c;
                    let dy = y as f32 + (sy as f32 + 0.5) / 4.0 - 0.5 - c;
                    // drawn at ~80% of the canvas so it sits at the same size as system icons
                    let r = (dx * dx + dy * dy).sqrt() / (n as f32 / 2.0 * 0.8);
                    let a = dy.atan2(dx);
                    let ring = (0.88..=1.0).contains(&r);
                    let hub = r <= 0.2;
                    // five swept blades between the hub and the ring
                    let blade = (0.2..0.8).contains(&r) && {
                        let sweep = a - r * 1.6;
                        let k = (sweep / (std::f32::consts::TAU / 5.0)).rem_euclid(1.0);
                        k < 0.32
                    };
                    if ring || hub || blade {
                        cov += 1.0;
                    }
                }
            }
            let i = (y * n + x) * 4;
            px[i + 3] = (cov / 16.0 * 255.0) as u8;
        }
    }
    Icon::from_rgba(px, n as u32, n as u32).expect("icon")
}

fn accel(s: &str) -> Option<Accelerator> {
    s.parse().ok()
}

impl Tray {
    /// Must run on the main thread (inside the app). `wake` is called on every menu
    /// click so the UI loop handles it right away.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Option<Tray> {
        let status = MenuItem::new("Reading sensors…", false, None);
        let modes = [
            CheckMenuItem::new("Apple Default", true, false, accel("CmdOrCtrl+1")),
            CheckMenuItem::new("Smart Cooling", true, false, accel("CmdOrCtrl+2")),
            CheckMenuItem::new("Full Blast", true, false, accel("CmdOrCtrl+3")),
        ];
        let open = MenuItem::new("Open Mac Smart Cooler", true, accel("CmdOrCtrl+O"));
        let quit = MenuItem::new("Quit Mac Smart Cooler", true, accel("CmdOrCtrl+Q"));
        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &modes[0],
            &modes[1],
            &modes[2],
            &PredefinedMenuItem::separator(),
            &open,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .ok()?;

        let (tx, rx): (Sender<_>, _) = channel();
        let tx = Mutex::new(tx);
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(e.id);
            }
            wake();
        }));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon_templated(glyph())
            .with_tooltip("Mac Smart Cooler")
            .build()
            .ok()?;
        Some(Tray {
            icon,
            status,
            modes,
            open,
            quit,
            rx,
            last_title: String::new(),
        })
    }

    /// Menu clicks since the last call.
    pub fn actions(&self) -> Vec<Action> {
        let mut out = Vec::new();
        while let Ok(id) = self.rx.try_recv() {
            if id == *self.open.id() {
                out.push(Action::Open);
            } else if id == *self.quit.id() {
                out.push(Action::Quit);
            } else if let Some(i) = self.modes.iter().position(|m| *m.id() == id) {
                out.push(Action::Mode(i));
            }
        }
        out
    }

    /// `title` sits next to the icon (e.g. "72°"); `selected` = checked mode, if any.
    pub fn update(&mut self, title: &str, status: &str, selected: Option<usize>) {
        if title != self.last_title {
            self.icon.set_title(Some(title));
            self.last_title = title.to_string();
        }
        if self.status.text() != status {
            self.status.set_text(status);
        }
        for (i, m) in self.modes.iter().enumerate() {
            // CheckMenuItem toggles itself on click; always restore the real state
            m.set_checked(selected == Some(i));
        }
    }
}
