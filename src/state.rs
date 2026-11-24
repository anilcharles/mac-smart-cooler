//! App-level state: the worker, the menu bar item, settings and fan history.
//!
//! It lives for the whole run, window or not, so the menu bar item keeps updating after the
//! window is closed. The window (`ui::Main`) only draws it and sends it the user's choices.
//! The logic is the egui front-end's, moved here unchanged.

use crate::helper;
use crate::sensors::HOT_KEY;
use crate::settings::{self, Settings};
use crate::smc::is_root;
use crate::tray::{Action, Tray};
use crate::worker::{Cmd, FanMode, Snapshot, Worker};
use futures::channel::mpsc::UnboundedSender;
use gpui::{App, AppContext, Context, Entity, Global};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Seconds of fan history drawn on each fan card.
pub const HIST_SECS: usize = 120;

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Preset {
    Automatic,
    Smart,
    FullBlast,
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Automatic, Preset::Smart, Preset::FullBlast];

    fn key(self) -> &'static str {
        match self {
            Preset::Automatic => "auto",
            Preset::Smart => "smart",
            Preset::FullBlast => "full",
            Preset::Custom => "custom",
        }
    }

    fn from_key(k: &str) -> Option<Preset> {
        match k {
            "smart" => Some(Preset::Smart),
            "full" => Some(Preset::FullBlast),
            _ => None,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Preset::Automatic => "Apple Default",
            Preset::Smart => "Smart Cooling",
            Preset::FullBlast => "Full Blast",
            Preset::Custom => "Custom",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Preset::Automatic => "macOS decides",
            Preset::Smart => "Quiet until it matters",
            Preset::FullBlast => "Every fan at maximum",
            Preset::Custom => "Per-fan settings",
        }
    }
}

static INSTALLING: AtomicBool = AtomicBool::new(false);

pub fn installing() -> bool {
    INSTALLING.load(Ordering::SeqCst)
}

/// Debug-only display overrides for the snapshot walkthrough (never sent anywhere).
#[derive(Default, Clone, Copy)]
pub struct Demo {
    pub error: bool,
    pub no_helper: bool,
    pub outdated: bool,
    pub installing: bool,
    pub preset: Option<Preset>,
}

pub struct Ctl {
    worker: Option<Worker>,
    tray: Option<Tray>,
    wake: UnboundedSender<()>,
    pub snap: Snapshot,
    pub helper: helper::Status,
    helper_checked: Option<Instant>,
    helper_checking: bool,
    pub s: Settings,
    saved: Settings,
    save_gen: u64,
    pending: Option<Preset>,
    /// settings handed to the helper since it became reachable (it keeps the system copy)
    helper_synced: bool,
    /// the preset just clicked, shown right away until the fans report it (or 4 s pass)
    chosen: Option<(Preset, Instant)>,
    /// asked once this launch to replace an outdated helper (so the latest cooling logic runs)
    auto_updated: bool,
    pub preset: Preset,
    pub model: String,
    pub model_id: String,
    /// smoothed rpm per fan, as displayed
    pub rpm: Vec<Option<f32>>,
    /// last ~2 minutes of each fan's rpm, one sample per second
    pub hist: Vec<VecDeque<f32>>,
    hist_at: Option<Instant>,
    /// Snapshot walkthrough: no menu bar item, no password prompts, no fan commands, no saving.
    pub frozen: bool,
    /// Showing made-up data (`sample.rs`) instead of this Mac's.
    pub sample: bool,
    pub demo: Demo,
}

/// The one `Ctl`, reachable from app-wide actions and the Dock's reopen event.
pub struct Shared(pub Entity<Ctl>);
impl Global for Shared {}

impl Ctl {
    /// `wake` is sent to whenever the worker has new readings or the menu bar item was used.
    pub fn new(wake: UnboundedSender<()>, frozen: bool, sample: bool) -> Self {
        let s = settings::load();
        let pending = if is_root() { Preset::from_key(&s.preset) } else { None };
        let worker = (!sample).then(|| {
            let w = wake.clone();
            Worker::spawn(move || {
                let _ = w.unbounded_send(());
            })
        });
        let tray = if frozen {
            None
        } else {
            let w = wake.clone();
            Tray::new(move || {
                let _ = w.unbounded_send(());
            })
        };
        Ctl {
            worker,
            tray,
            wake,
            snap: Snapshot { root: is_root(), ..Default::default() },
            helper: if sample { helper::Status::Installed } else { helper::status() },
            helper_checked: Some(Instant::now()),
            helper_checking: false,
            saved: s.clone(),
            s,
            save_gen: 0,
            pending,
            helper_synced: false,
            chosen: None,
            // never prompt for a password while just taking screenshots
            auto_updated: frozen,
            preset: Preset::Automatic,
            model: if sample { "MacBook Pro (16-inch, 2023)".into() } else { crate::load::machine_title() },
            model_id: if sample { "Mac15,7".into() } else { crate::smc::hw_model() },
            rpm: Vec::new(),
            hist: Vec::new(),
            hist_at: None,
            frozen,
            sample,
            demo: Demo::default(),
        }
    }

    pub fn temp(&self, c: f64) -> String {
        if self.s.fahrenheit {
            format!("{:.0}°F", c * 9.0 / 5.0 + 32.0)
        } else {
            format!("{c:.0}°C")
        }
    }

    /// Number only, with a degree sign (for big numerals).
    pub fn deg(&self, c: f64) -> String {
        if self.s.fahrenheit {
            format!("{:.0}°", c * 9.0 / 5.0 + 32.0)
        } else {
            format!("{c:.0}°")
        }
    }

    pub fn hot(&self) -> Option<f64> {
        self.snap.sensors.iter().find(|r| r.key == HOT_KEY).map(|r| r.value)
    }

    pub fn installing(&self) -> bool {
        installing() || self.demo.installing
    }

    pub fn smart_mode(&self) -> FanMode {
        FanMode::Smart { lo: self.s.smart_lo, hi: self.s.smart_hi }
    }

    fn preset_of(snap: &Snapshot) -> Preset {
        let all_auto = snap.modes.iter().all(|m| *m == FanMode::Auto);
        if all_auto && snap.fans.iter().any(|f| f.manual) {
            Preset::Custom // something else is driving the fans
        } else if snap.modes.is_empty() || all_auto {
            Preset::Automatic
        } else if snap.modes.iter().all(|m| matches!(m, FanMode::Smart { .. })) {
            Preset::Smart
        } else if snap.modes.iter().zip(&snap.fans).all(|(m, f)| matches!(m, FanMode::Constant(r) if *r >= f.max)) {
            Preset::FullBlast
        } else {
            Preset::Custom
        }
    }

    /// Hand a mode to the control loop (never during the snapshot walkthrough).
    pub fn send(&self, fan: usize, mode: FanMode) {
        if self.frozen {
            return;
        }
        if let Some(w) = &self.worker {
            w.send(Cmd::SetMode(fan, mode));
        }
    }

    fn apply_preset(&self, p: Preset) {
        for f in &self.snap.fans {
            let mode = match p {
                Preset::Automatic => FanMode::Auto,
                Preset::Smart => self.smart_mode(),
                Preset::FullBlast => FanMode::Constant(f.max),
                Preset::Custom => continue,
            };
            self.send(f.index, mode);
        }
    }

    pub fn choose_preset(&mut self, p: Preset, cx: &mut Context<Self>) {
        if p == self.preset || self.frozen {
            return;
        }
        self.s.preset = p.key().into();
        self.persist(cx);
        if self.snap.can_control || p == Preset::Automatic {
            self.chosen = Some((p, Instant::now()));
            self.preset = p;
            self.apply_preset(p);
        } else {
            // applied as soon as the helper is up; only ask for the password if the
            // helper is actually missing or outdated (not just still starting)
            self.pending = Some(p);
            if self.snap.root || matches!(helper::status(), helper::Status::NotInstalled | helper::Status::Outdated) {
                self.request_install();
            }
        }
        self.update_tray();
        cx.notify();
    }

    /// The Smart Cooling curve changed in Preferences: keep it live while it is being edited.
    pub fn smart_curve_changed(&mut self, cx: &mut Context<Self>) {
        if self.s.smart_hi < self.s.smart_lo + 5.0 {
            self.s.smart_hi = self.s.smart_lo + 5.0;
        }
        if self.preset == Preset::Smart {
            self.apply_preset(Preset::Smart);
        }
        self.persist(cx);
        cx.notify();
    }

    /// Install (or update) the background helper, off the UI thread. One password prompt.
    pub fn request_install(&self) {
        if self.frozen || INSTALLING.swap(true, Ordering::SeqCst) {
            return;
        }
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let _ = helper::install();
            // give launchd a moment to start it, then let the UI refresh its status
            std::thread::sleep(Duration::from_millis(1500));
            INSTALLING.store(false, Ordering::SeqCst);
            let _ = wake.unbounded_send(());
        });
    }

    pub fn request_uninstall(&self) {
        if self.frozen || INSTALLING.swap(true, Ordering::SeqCst) {
            return;
        }
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let _ = helper::uninstall();
            INSTALLING.store(false, Ordering::SeqCst);
            let _ = wake.unbounded_send(());
        });
    }

    /// Saves settings shortly after the last change, off the main thread (saving may ask
    /// the helper over its socket).
    pub fn persist(&mut self, cx: &mut Context<Self>) {
        if self.frozen || self.s == self.saved {
            return;
        }
        self.save_gen += 1;
        let gen = self.save_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let Ok(Some(s)) = this.update(cx, |c, _| {
                (c.save_gen == gen).then(|| {
                    c.saved = c.s.clone();
                    c.s.clone()
                })
            }) else {
                return;
            };
            cx.background_executor().spawn(async move { settings::save(&s) }).await;
        })
        .detach();
    }

    /// New readings (or a menu bar click): the egui `update()` minus the drawing.
    pub fn tick(&mut self, cx: &mut Context<Self>) {
        if let Some(w) = &self.worker {
            self.snap = w.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        }
        #[cfg(debug_assertions)]
        if self.sample {
            self.snap = crate::sample::snapshot();
        }
        if self.demo.error {
            self.snap.error = Some("Could not set Left side fan: SMC write failed (retrying in 30 s)".into());
        }
        if self.demo.no_helper {
            self.snap.can_control = false;
        }

        let actions = self.tray.as_ref().map(|t| t.actions()).unwrap_or_default();
        for a in actions {
            match a {
                Action::Mode(i) => self.choose_preset(Preset::ALL[i], cx),
                Action::Open => cx.defer(crate::ui::open_window),
                Action::Quit => cx.quit(),
            }
        }

        let installing = installing();
        if installing {
            self.helper_checked = None;
        } else if !self.sample
            && !self.helper_checking
            && self.helper_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(3))
        {
            // asks the helper over its socket: never on the main thread
            self.helper_checking = true;
            cx.spawn(async move |this, cx| {
                let status = cx.background_executor().spawn(async { helper::status() }).await;
                this.update(cx, |c, cx| {
                    c.helper_checking = false;
                    c.helper_checked = Some(Instant::now());
                    if c.helper != status {
                        c.helper = status;
                        cx.notify();
                    }
                    // The helper is what actually drives the fans; an old one keeps running old
                    // logic after the app is updated. Ask once per launch to replace it.
                    if c.helper == helper::Status::Outdated && !c.snap.root && !c.auto_updated {
                        c.auto_updated = true;
                        c.request_install();
                    }
                })
                .ok();
            })
            .detach();
        }
        if self.demo.outdated {
            self.helper = helper::Status::Outdated;
        } else if self.sample {
            self.helper = helper::Status::Installed;
        }
        if let Some(p) = self.pending {
            if self.snap.ready && self.snap.can_control {
                self.apply_preset(p);
                self.pending = None;
            }
        }
        // choices made before the helper was installed only reached the user's private copy
        if self.snap.can_control && !self.snap.root && !self.helper_synced && !self.frozen {
            let s = self.s.clone();
            cx.background_executor().spawn(async move { settings::save(&s) }).detach();
            self.helper_synced = true;
        }

        // Fan speed as shown: the tachometer wobbles +-20-50 rpm second to second even at a
        // fixed target, so show a ~3 s average (rounded to 10 rpm when drawn).
        if self.snap.ready && self.hist_at.is_none_or(|t| t.elapsed() >= Duration::from_millis(900)) {
            self.hist_at = Some(Instant::now());
            self.rpm.resize(self.snap.fans.len(), None);
            self.hist.resize(self.snap.fans.len(), Default::default());
            #[cfg(debug_assertions)]
            if self.sample && self.hist.iter().all(|h| h.is_empty()) {
                for (i, f) in self.snap.fans.iter().enumerate() {
                    self.hist[i] = crate::sample::history(i, f.actual as f32);
                }
            }
            for ((h, e), f) in self.hist.iter_mut().zip(self.rpm.iter_mut()).zip(&self.snap.fans) {
                let a = f.actual as f32;
                // jump straight to big changes (a real speed change), smooth the small wobble
                let v = match *e {
                    Some(p) if (a - p).abs() < 150.0 => p + (a - p) / 3.0,
                    _ => a,
                };
                *e = Some(v);
                h.push_back(v);
                while h.len() > HIST_SECS {
                    h.pop_front();
                }
            }
        }

        let mut preset = Self::preset_of(&self.snap);
        if let Some((p, t)) = self.chosen {
            if p == preset || t.elapsed() > Duration::from_secs(4) {
                self.chosen = None;
            } else {
                preset = p;
            }
        }
        self.preset = self.demo.preset.unwrap_or(preset);
        self.update_tray();
        cx.notify();
    }

    /// Menu bar item: temperature next to the icon, one status line, the checked mode.
    fn update_tray(&mut self) {
        if self.tray.is_none() {
            return;
        }
        let hot = self.hot();
        let title = hot.map(|t| self.deg(t)).unwrap_or_default();
        // one short line: "62°C · Fans off · Quiet"
        let fans = if self.snap.fans.iter().all(|f| f.actual < 1.0) {
            "Fans off".to_string()
        } else {
            format!("{} rpm", self.snap.fans.iter().map(|f| thousands(f.actual)).collect::<Vec<_>>().join(" / "))
        };
        let state = match self.preset {
            Preset::Automatic => "macOS".to_string(),
            Preset::Smart => {
                self.snap.activity.smart.as_deref().and_then(|w| w.split(':').next()).unwrap_or("Starting").to_string()
            }
            Preset::FullBlast => "Full Blast".into(),
            Preset::Custom => "Custom".into(),
        };
        let status = match hot {
            Some(t) => format!("{} · {fans} · {state}", self.temp(t)),
            None => "Reading sensors…".into(),
        };
        let sel = Preset::ALL.iter().position(|p| *p == self.preset);
        if let Some(t) = &mut self.tray {
            t.update(&title, &status, sel);
        }
    }
}

/// 1234567 -> "1,234,567".
pub fn thousands(v: f64) -> String {
    let n = v.max(0.0).round() as u64;
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Starts the app-level state and the loop that feeds it. Returns the entity.
pub fn start(cx: &mut App, frozen: bool, sample: bool) -> Entity<Ctl> {
    use futures::StreamExt;
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
    let ticker = tx.clone();
    let ctl = cx.new(|_| Ctl::new(tx, frozen, sample));
    if sample {
        // no worker: a steady second-by-second tick instead
        cx.spawn(async move |cx| loop {
            if ticker.unbounded_send(()).is_err() {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(1)).await;
        })
        .detach();
    }
    let c = ctl.clone();
    cx.spawn(async move |cx| {
        while rx.next().await.is_some() {
            while rx.try_recv().is_ok() {} // one update for a burst of wake-ups
            if c.update(cx, |c, cx| c.tick(cx)).is_err() {
                break;
            }
        }
    })
    .detach();
    cx.set_global(Shared(ctl.clone()));
    ctl
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fans::FanInfo;

    fn fan(i: usize, manual: bool) -> FanInfo {
        FanInfo { index: i, name: format!("Fan {i}"), min: 1200., max: 6000., actual: 2000., target: 2000., manual }
    }

    #[test]
    fn preset_follows_the_fan_modes() {
        let mut s = Snapshot { fans: vec![fan(0, false), fan(1, false)], ..Default::default() };
        s.modes = vec![FanMode::Auto, FanMode::Auto];
        assert_eq!(Ctl::preset_of(&s), Preset::Automatic);
        s.fans[0].manual = true;
        assert_eq!(Ctl::preset_of(&s), Preset::Custom, "another app is driving a fan");
        s.modes = vec![FanMode::Smart { lo: 60., hi: 88. }; 2];
        assert_eq!(Ctl::preset_of(&s), Preset::Smart);
        s.modes = vec![FanMode::Constant(6000.), FanMode::Constant(6000.)];
        assert_eq!(Ctl::preset_of(&s), Preset::FullBlast);
        s.modes = vec![FanMode::Constant(3000.), FanMode::Auto];
        assert_eq!(Ctl::preset_of(&s), Preset::Custom);
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(thousands(0.), "0");
        assert_eq!(thousands(999.4), "999");
        assert_eq!(thousands(2450.), "2,450");
        assert_eq!(thousands(1234567.), "1,234,567");
        assert_eq!(thousands(-5.), "0");
    }
}
