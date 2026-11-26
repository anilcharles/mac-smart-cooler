//! The window. A main column (current mode, live load, modes, fans) and a sidebar on the
//! right that shows either the sensors or the busiest processes (and what was busy each time
//! the fans came on), with dialogs floating over it. Everything it shows comes from `state::Ctl`; every choice goes back to it.

use crate::charts::{ring_gauge, sparkline};
use crate::elided::elided;
use crate::fans::FanInfo;
use crate::helper;
use crate::procs::{self, FanEvent, Owner, Sort, Usage};
use crate::sensors::{Reading, HOT_KEY};
use crate::settings::Settings;
use crate::state::{thousands, Ctl, Preset, Shared, HIST_SECS};
use crate::theme::*;
use crate::ui_prefs::{ThemeMode, UiPrefs};
use crate::widgets::*;
use crate::worker::{FanMode, Snapshot};
use gpui::{
    actions, canvas, div, linear_color_stop, linear_gradient, point, prelude::*, relative, App, Bounds, ClickEvent, Context, Div, Entity,
    FocusHandle, FontWeight, Global, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, SharedString, Size, Stateful, Subscription, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowHandle, WindowOptions,
};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

actions!(
    mac_smart_cooler,
    [
        Cancel, Confirm, Quit, About, Preferences, CloseWindow, Minimize, ThemeLight, ThemeDark, ThemeAuto, ZoomIn,
        ZoomOut, ZoomReset, ModeAuto, ModeSmart, ModeFull
    ]
);

pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// The window's rem size at 100% zoom; `pt()` in theme.rs divides by the same number.
const BASE_REM: f32 = 16.;

#[derive(PartialEq, Clone, Copy)]
enum DialogKind {
    Constant,
    Sensor,
}

struct CustomDialog {
    fan: usize,
    kind: DialogKind,
    rpm: f64,
    sensor: String,
    lo: f64,
    hi: f64,
}

#[derive(PartialEq, Clone, Copy)]
enum Modal {
    None,
    Fan,
    Prefs,
    About,
}

/// Something a popup menu entry does.
#[derive(Clone)]
enum Cmd {
    Sensor(String),
}

struct PopItem {
    label: SharedString,
    checked: bool,
    cmd: Cmd,
}

/// A small menu shown at a point in the window.
struct Popup {
    pos: Point<Pixels>,
    width: f32,
    items: Vec<PopItem>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum SliderId {
    Rpm,
    SmartLo,
    SmartHi,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StepId {
    Lo,
    Hi,
}

/// What a frame draws, copied out of `Ctl` so the view can borrow `cx` freely.
struct V {
    snap: Snapshot,
    s: Settings,
    preset: Preset,
    helper: helper::Status,
    installing: bool,
    rpm: Vec<Option<f32>>,
    hist: Vec<VecDeque<f32>>,
    model: String,
    model_id: String,
    procs: procs::Latest,
    spins: Vec<FanEvent>,
}

impl V {
    fn of(c: &Ctl) -> Self {
        V {
            snap: c.snap.clone(),
            s: c.s.clone(),
            preset: c.preset,
            helper: c.helper,
            installing: c.installing(),
            rpm: c.rpm.clone(),
            hist: c.hist.clone(),
            model: c.model.clone(),
            model_id: c.model_id.clone(),
            procs: c.procs.clone(),
            spins: c.spins.events.iter().cloned().collect(),
        }
    }

    fn temp(&self, c: f64) -> String {
        if self.s.fahrenheit {
            format!("{:.0}°F", c_to_f(c))
        } else {
            format!("{c:.0}°C")
        }
    }

    fn deg(&self, c: f64) -> String {
        if self.s.fahrenheit {
            format!("{:.0}°", c_to_f(c))
        } else {
            format!("{c:.0}°")
        }
    }

    fn hot(&self) -> Option<f64> {
        self.snap.sensors.iter().find(|r| r.key == HOT_KEY).map(|r| r.value)
    }
}

fn c_to_f(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

/// Friendlier names for a few sensors (display only; the helper is unaffected).
fn display_name(r: &Reading) -> &str {
    match r.key.as_str() {
        "TaLP" => "Case Air Left",
        "TaRF" => "Case Air Right",
        "TW0P" => "Wi-Fi Module",
        HOT_KEY => "Hottest Core",
        _ => &r.name,
    }
}

fn group_of(r: &Reading) -> usize {
    if !r.named {
        4
    } else if r.name.starts_with("CPU") || r.key == HOT_KEY {
        0
    } else if r.name.starts_with("GPU") {
        1
    } else if r.name.starts_with("Battery") {
        2
    } else {
        3
    }
}
const GROUPS: [(&str, &str); 5] =
    [("Processor", "chip"), ("Graphics", "gpu"), ("Battery", "battery"), ("System", "laptop"), ("Unidentified", "thermometer")];

/// The open window, if any (the app keeps running without one).
struct OpenWindow(Option<WindowHandle<Main>>);
impl Global for OpenWindow {}

/// Shows the window: brings the open one forward, or opens a new one on the same state.
pub fn open_window(cx: &mut App) {
    if let Some(h) = cx.try_global::<OpenWindow>().and_then(|w| w.0) {
        if h.update(cx, |_, window, _| window.activate_window()).is_ok() {
            cx.activate(true);
            return;
        }
    }
    let Some(ctl) = cx.try_global::<Shared>().map(|s| s.0.clone()) else { return };
    let prefs = UiPrefs::load();
    let bounds = Bounds::centered(None, window_size(WINDOW_SIZE, prefs.zoom), cx);
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Mac Smart Cooler".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(gpui::px(18.), gpui::px(18.))),
            }),
            window_min_size: Some(window_size(WINDOW_MIN, prefs.zoom)),
            app_id: Some("local.macsmartcooler.app".into()),
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                #[allow(unused_mut)]
                let mut main = Main::new(ctl, prefs, window, cx);
                // Debug builds only: a released app must not be steerable through its environment.
                #[cfg(debug_assertions)]
                if let Ok(dir) = std::env::var("MACSMARTCOOLER_SNAPSHOT_DIR") {
                    main.start_demo(dir, window, cx);
                }
                main
            })
        },
    );
    if let Ok(h) = handle {
        cx.set_global(OpenWindow(Some(h)));
    }
    cx.activate(true);
}

pub struct Main {
    ctl: Entity<Ctl>,
    focus: FocusHandle,
    prefs: UiPrefs,
    modal: Modal,
    dialog: Option<CustomDialog>,
    popup: Option<Popup>,
    /// the slider being dragged, and where each slider's track was last drawn
    drag: Option<SliderId>,
    tracks: HashMap<SliderId, Rc<Cell<Bounds<Pixels>>>>,
    /// a stepper button held down (repeats), with a counter so an old repeat loop stops
    hold: Option<(StepId, i32, u64)>,
    hold_gen: u64,
    /// process table: the column it is sorted by, and every fan spin-up listed (not just the latest)
    sort: Sort,
    all_spins: bool,
    app_icon: Arc<gpui::Image>,
    _subs: Vec<Subscription>,
}

impl Main {
    fn new(ctl: Entity<Ctl>, prefs: UiPrefs, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let entity = cx.entity().downgrade();
        let appearance = window.observe_window_appearance(move |_, cx| {
            entity.update(cx, |_, cx| cx.notify()).ok();
        });
        let observe = cx.observe(&ctl, |_, _, cx| cx.notify());
        window.set_rem_size(gpui::px(BASE_REM * prefs.zoom));
        Main {
            ctl,
            focus,
            prefs,
            modal: Modal::None,
            dialog: None,
            popup: None,
            drag: None,
            tracks: HashMap::new(),
            hold: None,
            hold_gen: 0,
            sort: Sort::Cpu,
            all_spins: false,
            app_icon: Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, ICON_PNG.to_vec())),
            _subs: vec![appearance, observe],
        }
    }

    /* ================================================================
       Actions
       ================================================================ */

    fn set_theme(&mut self, mode: ThemeMode, cx: &mut Context<Self>) {
        self.prefs.theme = mode;
        self.prefs.save();
        cx.notify();
    }

    /// The sidebar's tab: sensors or processes.
    fn show_processes(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.prefs.processes != on {
            self.prefs.processes = on;
            self.prefs.save();
            cx.notify();
        }
    }

    /// Makes the whole interface bigger or smaller. Every size is in rems, so changing the
    /// window's rem size scales text, icons and spacing together.
    fn set_zoom(&mut self, zoom: f32, window: &mut Window, cx: &mut Context<Self>) {
        if (zoom - self.prefs.zoom).abs() < 0.001 {
            return;
        }
        let old = self.prefs.zoom;
        self.prefs.zoom = zoom;
        self.prefs.save();
        self.popup = None; // it was placed in pixels of the old size
        window.set_rem_size(gpui::px(BASE_REM * zoom));

        let min = window_size(WINDOW_MIN, zoom);
        crate::native::set_min_content_size(window, min.width.into(), min.height.into());
        // The window grows and shrinks with its content, so the layout stays as it was.
        if !window.is_fullscreen() && !window.is_maximized() {
            let now = window.viewport_size();
            let limit = window_size((f32::MAX, f32::MAX), 1.);
            let k = zoom / old;
            window.resize(gpui::size(
                (now.width * k).min(limit.width).max(min.width),
                (now.height * k).min(limit.height).max(min.height),
            ));
        }
        cx.notify();
    }

    /// One zoom level up (`delta > 0`) or down.
    fn step_zoom(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let next = if delta > 0 {
            ZOOM_STEPS.iter().copied().find(|s| *s > self.prefs.zoom + 0.001)
        } else {
            ZOOM_STEPS.iter().rev().copied().find(|s| *s < self.prefs.zoom - 0.001)
        };
        if let Some(zoom) = next {
            self.set_zoom(zoom, window, cx);
        }
    }

    fn choose(&mut self, p: Preset, cx: &mut Context<Self>) {
        self.ctl.update(cx, |c, cx| c.choose_preset(p, cx));
    }

    fn settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        self.ctl.update(cx, |c, cx| {
            f(&mut c.s);
            c.persist(cx);
            cx.notify();
        });
    }

    fn open_dialog(&mut self, fan: &FanInfo, mode: &FanMode, cx: &mut Context<Self>) {
        if !(fan.max > fan.min) {
            return;
        }
        let s = &self.ctl.read(cx).s;
        let mut d = CustomDialog {
            fan: fan.index,
            kind: DialogKind::Constant,
            rpm: ((fan.min + fan.max) / 2.0 / 100.0).round() * 100.0,
            sensor: HOT_KEY.to_string(),
            lo: s.smart_lo,
            hi: s.smart_hi,
        };
        match mode {
            FanMode::Auto | FanMode::Smart { .. } => {}
            FanMode::Constant(r) => d.rpm = (*r / 50.0).round() * 50.0,
            FanMode::Sensor { key, lo, hi } => {
                d.kind = DialogKind::Sensor;
                d.sensor = key.clone();
                d.lo = *lo;
                d.hi = *hi;
            }
        }
        self.dialog = Some(d);
        self.popup = None;
        self.modal = Modal::Fan;
        cx.notify();
    }

    fn apply_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(d) = self.dialog.take() else { return };
        let mode = match d.kind {
            DialogKind::Constant => FanMode::Constant(d.rpm),
            DialogKind::Sensor => FanMode::Sensor { key: d.sensor.clone(), lo: d.lo, hi: d.hi.max(d.lo + 1.0) },
        };
        self.ctl.read(cx).send(d.fan, mode);
        self.modal = Modal::None;
        cx.notify();
    }

    fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.modal = Modal::None;
        self.dialog = None;
        self.popup = None;
        self.drag = None;
        self.hold = None;
        cx.notify();
    }

    fn on_cancel(&mut self, cx: &mut Context<Self>) {
        if self.popup.take().is_some() {
            cx.notify();
        } else if self.modal != Modal::None {
            self.close_modal(cx);
        }
    }

    fn run_cmd(&mut self, cmd: Cmd, cx: &mut Context<Self>) {
        match cmd {
            Cmd::Sensor(key) => {
                if let Some(d) = &mut self.dialog {
                    d.sensor = key;
                }
                cx.notify();
            }
        }
    }

    /// A slider moved to window position `pos`.
    fn slide(&mut self, id: SliderId, pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(b) = self.tracks.get(&id).map(|c| c.get()) else { return };
        let w = f32::from(b.size.width);
        if w <= 0. {
            return;
        }
        let frac = (f32::from(pos.x - b.origin.x) / w).clamp(0., 1.) as f64;
        match id {
            SliderId::Rpm => {
                let Some(fan) = self.dialog.as_ref().map(|d| d.fan) else { return };
                let Some((min, max)) = self.ctl.read(cx).snap.fans.get(fan).map(|f| (f.min, f.max)) else { return };
                if let Some(d) = &mut self.dialog {
                    d.rpm = (((min + frac * (max - min)) / 50.0).round() * 50.0).clamp(min, max);
                }
                cx.notify();
            }
            SliderId::SmartLo | SliderId::SmartHi => {
                let v = if id == SliderId::SmartLo { 45. + frac * 40. } else { 60. + frac * 40. }.round();
                self.ctl.update(cx, |c, cx| {
                    let slot = if id == SliderId::SmartLo { &mut c.s.smart_lo } else { &mut c.s.smart_hi };
                    if *slot != v {
                        *slot = v;
                        c.smart_curve_changed(cx);
                    }
                });
            }
        }
    }

    /// One step of a temperature stepper: 1° in the unit shown.
    fn step(&mut self, id: StepId, dir: i32, cx: &mut Context<Self>) {
        let f = self.ctl.read(cx).s.fahrenheit;
        let Some(d) = &mut self.dialog else { return };
        let slot = if id == StepId::Lo { &mut d.lo } else { &mut d.hi };
        let next = if f {
            ((c_to_f(*slot).round() + dir as f64) - 32.0) * 5.0 / 9.0
        } else {
            slot.round() + dir as f64
        };
        *slot = next.clamp(20.0, 110.0);
        cx.notify();
    }

    fn start_hold(&mut self, id: StepId, dir: i32, cx: &mut Context<Self>) {
        self.step(id, dir, cx);
        self.hold_gen += 1;
        let gen = self.hold_gen;
        self.hold = Some((id, dir, gen));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            loop {
                let held = this
                    .update(cx, |this, cx| {
                        let still = this.hold.is_some_and(|(_, _, g)| g == gen);
                        if still {
                            this.step(id, dir, cx);
                        }
                        still
                    })
                    .unwrap_or(false);
                if !held {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(70)).await;
            }
        })
        .detach();
    }

    /* ================================================================
       Title bar (transparent; the traffic lights sit on the left)
       ================================================================ */

    fn titlebar(&self, v: &V, t: &Theme, cx: &mut Context<Self>) -> Div {
        let prefs_open = self.modal == Modal::Prefs;
        div()
            .h(TITLEBAR)
            .flex_none()
            // the traffic lights are drawn by macOS at a fixed size, so their space is real pixels
            .pl(gpui::px(86.))
            .pr(pt(14.))
            .pt(pt(1.))
            .flex()
            .items_center()
            .gap(pt(8.))
            .child(div().text_size(pt(14.)).child("Mac Smart Cooler"))
            .child(
                div()
                    .id("model")
                    .text_size(pt(12.))
                    .text_color(t.text_3)
                    .tooltip(tip(format!("Model identifier: {}", v.model_id), t))
                    .child(v.model.clone()),
            )
            .child(div().flex_1())
            .child(self.control_status(v, t, cx))
            .child(
                div()
                    .id("prefs")
                    .size(pt(28.))
                    .flex_none()
                    .rounded_full()
                    .bg(if prefs_open { t.hover } else { t.surface_2 })
                    .border_1()
                    .border_color(t.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.hover))
                    .tooltip(tip("Preferences", t))
                    .child(icon("gear", 15., t.text_2))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let open = this.modal == Modal::Prefs;
                        this.close_modal(cx);
                        if !open {
                            this.modal = Modal::Prefs;
                        }
                    })),
            )
    }

    /// Fan control on, or the one button that gets it there.
    fn control_status(&self, v: &V, t: &Theme, cx: &mut Context<Self>) -> Div {
        let root = v.snap.root;
        let install = |id: &'static str, label: &'static str, hint: &'static str, cx: &mut Context<Self>| {
            button(id, label, None, Btn::Primary, true, t)
                .tooltip(tip(hint, t))
                .on_click(cx.listener(|this, _, _, cx| this.ctl.read(cx).request_install()))
        };
        let el = div().flex().items_center().gap(pt(8.));
        if v.installing {
            el.child(status_pill("Waiting for password…", t.warning))
        } else if v.helper == helper::Status::Outdated && !root {
            el.child(install(
                "update-helper",
                "Update helper",
                "The background helper is older than this app. One password prompt updates it.",
                cx,
            ))
        } else if v.helper == helper::Status::Unreachable && !root {
            el.child(install(
                "repair-helper",
                "Repair helper",
                "The background helper isn't answering. One password prompt reinstalls it.",
                cx,
            ))
        } else if v.snap.can_control {
            el.child(status_pill("Fan control on", t.success))
        } else {
            el.child(status_pill(if v.snap.ready { "Monitoring only" } else { "Starting…" }, t.warning)).child(install(
                "enable",
                "Enable fan control",
                "Installs a small background helper. Asks for your password once, never again.",
                cx,
            ))
        }
    }

    /* ================================================================
       Main column
       ================================================================ */

    /// Big status card: hottest-core ring gauge, what the current mode is doing, live load.
    fn hero(&self, v: &V, t: &Theme, cx: &mut Context<Self>) -> Div {
        let a = &v.snap.activity;
        let hot = v.hot();
        let arrow = if a.trend > 0.5 {
            "↑"
        } else if a.trend < -0.5 {
            "↓"
        } else {
            "→"
        };
        let trend = if v.s.fahrenheit { a.trend * 1.8 } else { a.trend };
        let gauge = div()
            .relative()
            .size(pt(148.))
            .flex_none()
            .child(div().absolute().inset_0().child(ring_gauge(hot, *t)))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .when_some(hot, |el, h| {
                        el.child(num(40.).line_height(pt(46.)).child(v.deg(h)))
                            .child(num(12.).text_color(t.text_2).child(format!("{arrow} {trend:+.1}°/min")))
                    })
                    .when(hot.is_none(), |el| el.child(num(36.).text_color(t.text_3).child("—"))),
            )
            .when(hot.is_some(), |el| {
                el.child(
                    div()
                        .absolute()
                        .bottom(pt(6.))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .text_size(pt(10.))
                        .text_color(t.text_3)
                        .child("HOTTEST CORE"),
                )
            });

        let (label, title, detail): (&str, String, String) = match v.preset {
            Preset::Automatic => (
                "APPLE DEFAULT",
                "macOS is in control".into(),
                "Fans follow Apple's own curve and often stay off until the chip is already hot.".into(),
            ),
            Preset::Smart => {
                let why = a.smart.clone().unwrap_or_else(|| "Quiet: starting up".into());
                let (state, d) = why.split_once(": ").unwrap_or((why.as_str(), ""));
                let mut d = d.to_string();
                if let Some(f) = d.get(0..1) {
                    d = f.to_uppercase() + &d[1..];
                }
                ("SMART COOLING", state.to_string(), d)
            }
            Preset::FullBlast => ("FULL BLAST", "Maximum airflow".into(), "Every fan is running at full speed.".into()),
            Preset::Custom => ("CUSTOM", "Per-fan settings".into(), "Each fan follows its own setting below.".into()),
        };

        let mut items: Vec<(&str, String, f32, Hsla)> = vec![("CPU", format!("{:.0}%", a.cpu * 100.0), a.cpu as f32, t.load(a.cpu))];
        if let Some(g) = a.gpu {
            items.push(("GPU", format!("{:.0}%", g * 100.0), g as f32, t.load(g)));
        }
        if let Some(c) = a.chassis {
            items.push(("PALM REST", v.temp(c), ((c - 20.0) / 25.0) as f32, t.temp(c + 12.0)));
        }
        if let Some(b) = a.body {
            items.push(("CASE AIR", v.temp(b), ((b - 28.0) / 36.0) as f32, t.temp(b + 20.0)));
        }
        let meters = items.into_iter().enumerate().map(|(i, (label, value, frac, col))| {
            // CPU and GPU open the process table: what is behind the number
            let load = matches!(label, "CPU" | "GPU");
            div()
                .id(("meter", i))
                .when(load, |el| {
                    el.cursor_pointer()
                        .tooltip(tip(format!("See which processes are using the {label}"), t))
                        .on_click(cx.listener(|this, _, _, cx| this.show_processes(true, cx)))
                })
                .flex_1()
                .min_w(pt(60.))
                .child(div().text_size(pt(10.)).text_color(t.text_3).child(label))
                .child(num(18.).line_height(pt(24.)).child(value))
                .child(
                    div()
                        .mt(pt(4.))
                        .h(pt(4.))
                        .rounded(pt(2.))
                        .bg(t.well)
                        .child(div().h_full().w(relative(frac.clamp(0.05, 1.))).rounded(pt(2.)).bg(gradient_h(mix(col, t.surface, 0.35), col))),
                )
        });

        panel(t)
            .p(pt(22.))
            .flex()
            .items_center()
            .gap(pt(20.))
            .child(gauge)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(pt(4.))
                    .child(div().text_size(pt(11.)).text_color(t.cyan).child(label))
                    .child(div().text_size(pt(26.)).line_height(pt(32.)).child(title))
                    .child(div().text_color(t.text_2).child(detail))
                    .child(div().mt(pt(12.)).flex().gap(pt(18.)).children(meters)),
            )
    }

    /// The three presets as large cards.
    fn mode_cards(&self, v: &V, t: &Theme, cx: &mut Context<Self>) -> Div {
        let cards = Preset::ALL.into_iter().enumerate().map(|(i, p)| {
            let sel = v.preset == p;
            let glyph = ["check-circle", "gauge", "wind"][i];
            let (fill, hover) = if sel { (mix(t.surface, t.accent, 0.16), mix(t.surface, t.accent, 0.16)) } else { (t.surface, mix(t.surface, t.surface_2, 0.6)) };
            div()
                .id(("mode", i))
                .flex_1()
                .min_w_0()
                .h(pt(64.))
                .px(pt(11.))
                .rounded(pt(14.))
                .bg(fill)
                .border_1()
                .border_color(if sel { t.accent } else { t.border })
                .flex()
                .items_center()
                .gap(pt(12.))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .when(!sel, |el| el.on_click(cx.listener(move |this, _, _, cx| this.choose(p, cx))))
                .child(
                    div()
                        .size(pt(34.))
                        .flex_none()
                        .rounded_full()
                        .bg(if sel { mix(t.surface_2, t.accent, 0.25) } else { t.surface_2 })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(glyph, 19., if sel { t.cyan } else { t.text_2 })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().ellipsis().text_size(pt(14.)).child(elided(p.title())))
                        .child(
                            div()
                                .ellipsis()
                                .text_size(pt(11.5))
                                .text_color(if sel { t.text_2 } else { t.text_3 })
                                .child(elided(p.subtitle())),
                        ),
                )
        });
        div().flex().gap(pt(12.)).children(cards)
    }

    fn fans(&self, v: &V, t: &Theme, cx: &mut Context<Self>) -> Div {
        let row = div().flex_1().min_h(pt(150.)).flex().gap(pt(12.));
        if !v.snap.ready || v.snap.fans.is_empty() {
            let text = if v.snap.ready { "No fans found (fanless Mac?)" } else { "Reading fans…" };
            return row.child(panel(t).flex_1().p(pt(18.)).text_color(t.text_2).child(text));
        }
        let cards: Vec<Div> = v.snap.fans.iter().map(|f| self.fan_card(f, v, t, cx)).collect();
        row.children(cards)
    }

    fn fan_card(&self, fan: &FanInfo, v: &V, t: &Theme, cx: &mut Context<Self>) -> Div {
        let mode = v.snap.modes.get(fan.index).cloned().unwrap_or(FanMode::Auto);
        let status = match &mode {
            FanMode::Auto if fan.manual => "Set by another app".to_string(),
            FanMode::Auto => "macOS".into(),
            FanMode::Constant(r) => format!("Fixed {} rpm", thousands(*r)),
            FanMode::Smart { .. } if fan.manual => "Smart Cooling · active".into(),
            FanMode::Smart { .. } => "Smart Cooling · resting".into(),
            FanMode::Sensor { key, lo, hi } => {
                let name = v.snap.sensors.iter().find(|r| &r.key == key).map(display_name).unwrap_or(key);
                format!("{name} {}–{}", v.temp(*lo), v.temp(*hi))
            }
        };
        let is_auto = mode == FanMode::Auto && !fan.manual;
        let is_smart = matches!(mode, FanMode::Smart { .. });
        let sel = if is_auto { 0 } else if is_smart { 1 } else { 2 };
        let idx = fan.index;
        let switcher = segmented(t).w(pt(186.)).min_w(pt(168.)).flex_shrink().children(["Auto", "Smart", "Custom"].into_iter().enumerate().map(|(i, label)| {
            let f = fan.clone();
            let m = mode.clone();
            segment(("fan-mode", idx * 8 + i), label, sel == i, t).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                match i {
                    0 if !is_auto => this.ctl.read(cx).send(idx, FanMode::Auto),
                    1 if !is_smart => {
                        let c = this.ctl.read(cx);
                        c.send(idx, c.smart_mode());
                    }
                    2 => this.open_dialog(&f, &m, cx),
                    _ => {}
                }
            }))
        }));

        let smooth = v.rpm.get(idx).copied().flatten().unwrap_or(fan.actual as f32);
        let rpm = (smooth / 10.0).round() * 10.0;
        let frac = if fan.max > 0. { (rpm as f64 / fan.max).clamp(0., 1.) as f32 } else { 0. };
        let min_frac = if fan.max > 0. { (fan.min / fan.max).clamp(0., 1.) as f32 } else { 0. };
        let hist = v.hist.get(idx).cloned().unwrap_or_default();
        let spinning = fan.actual > 0.;

        panel(t)
            .flex_1()
            .min_w_0()
            .p(pt(18.))
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(pt(10.))
                    .child(
                        div()
                            .size(pt(32.))
                            .flex_none()
                            .rounded_full()
                            .bg(t.well)
                            .border_1()
                            .border_color(t.border)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("fan", 22., if spinning { t.cyan } else { t.text_3 })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(pt(84.))
                            .flex()
                            .flex_col()
                            .child(div().ellipsis().text_size(pt(14.)).child(elided(fan.name.clone())))
                            .child(div().ellipsis().text_size(pt(11.5)).text_color(t.text_3).child(elided(status))),
                    )
                    .child(switcher),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(pt(24.))
                    .mt(pt(18.))
                    .mb(pt(10.))
                    .child(div().absolute().inset_0().child(sparkline(hist, HIST_SECS, fan.max as f32, *t)))
                    .child(div().absolute().top_0().left_0().text_size(pt(9.5)).text_color(t.text_3).child("LAST 2 MIN")),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(pt(6.))
                    .child(num(38.).line_height(pt(42.)).child(thousands(rpm as f64)))
                    .child(div().text_size(pt(13.)).text_color(t.text_3).child("rpm"))
                    .child(div().flex_1())
                    .when(fan.max > 0., |el| el.child(num(13.).text_color(t.text_2).child(format!("{:.0}%", frac * 100.)))),
            )
            .when(fan.max > 0., |el| {
                el.child(
                    div()
                        .relative()
                        .mt(pt(8.))
                        .h(pt(6.))
                        .rounded(pt(3.))
                        .bg(t.well)
                        .child(div().h_full().w(relative(frac.max(0.02))).rounded(pt(3.)).bg(gradient_h(t.accent, t.cyan)))
                        .child(div().absolute().top(pt(-3.)).left(relative(min_frac)).w(pt(1.5)).h(pt(12.)).rounded(pt(1.)).bg(t.text_2)),
                )
                .child(
                    div()
                        .mt(pt(6.))
                        .flex()
                        .child(num(11.).text_color(t.text_3).child(format!("min {}", thousands(fan.min))))
                        .child(div().flex_1())
                        .child(num(11.).text_color(t.text_3).child(format!("max {}", thousands(fan.max)))),
                )
            })
    }

    /* ================================================================
       Sensors (right column)
       ================================================================ */

    /// The right-hand column: a Sensors | Processes switch over the chosen list.
    fn sidebar(&self, v: &V, t: &Theme, cx: &mut Context<Self>) -> Div {
        let on = self.prefs.processes;
        let tabs = segmented(t)
            .flex_none()
            .child(segment("side-sensors", "Sensors", !on, t).on_click(cx.listener(|this, _, _, cx| this.show_processes(false, cx))))
            .child(segment("side-procs", "Processes", on, t).on_click(cx.listener(|this, _, _, cx| this.show_processes(true, cx))));
        div().w(SENSORS_W).flex_none().h_full().pr(pt(20.)).pt(pt(6.)).pb(pt(20.)).child(
            panel(t)
                .size_full()
                .p(pt(18.))
                .pt(pt(14.))
                .flex()
                .flex_col()
                .child(tabs)
                .map(|el| if on { el.child(self.processes(v, t, cx)) } else { el.child(self.sensors(v, t)) }),
        )
    }

    fn sensors(&self, v: &V, t: &Theme) -> Div {
        let shown = v.snap.sensors.iter().filter(|r| r.named || v.s.show_unnamed).count();
        let mut list = div().id("sensor-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();
        for (g, (title, _)) in GROUPS.iter().enumerate() {
            if g == 4 && !v.s.show_unnamed {
                continue;
            }
            let rows: Vec<&Reading> = v.snap.sensors.iter().filter(|r| group_of(r) == g).collect();
            if rows.is_empty() {
                continue;
            }
            list = list.child(div().mt(pt(10.)).mb(pt(2.)).text_size(pt(10.5)).text_color(t.text_3).child(title.to_uppercase()));
            let n = rows.len();
            for (i, r) in rows.into_iter().enumerate() {
                let color = t.temp(r.value);
                list = list.child(
                    div()
                        .id(SharedString::from(format!("sensor-{}", r.key)))
                        .relative()
                        .flex_none()
                        .h(pt(30.))
                        .pl(pt(1.))
                        .pr(pt(6.))
                        .rounded(pt(6.))
                        .flex()
                        .items_center()
                        .gap(pt(10.))
                        .hover(|s| s.bg(t.hover))
                        .tooltip(tip(format!("SMC key {}", r.key), t))
                        .child(div().size(pt(6.)).flex_none().rounded_full().bg(color))
                        .child(div().flex_1().min_w_0().ellipsis().text_color(mix(t.text_2, t.text, 0.6)).child(elided(display_name(r).to_string())))
                        .child(num(13.).text_color(color).child(v.temp(r.value)))
                        .when(i + 1 < n, |el| el.child(div().absolute().bottom_0().left(pt(16.)).right_0().h(pt(1.)).bg(t.border.opacity(0.6)))),
                );
            }
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(v.snap.ready, |el| {
                el.child(div().mt(pt(10.)).text_size(pt(11.5)).text_color(t.text_3).child(format!("{shown} temperature sensors")))
            })
            .when(!v.snap.ready, |el| {
                el.child(div().mt(pt(12.)).flex().items_center().gap(pt(8.)).child(spinner("sensors-loading", 14., t)).child(div().text_color(t.text_2).child("Reading sensors…")))
            })
            .child(list)
    }

    /// What is using the Mac right now, and what was busy each time the fans came on.
    fn processes(&self, v: &V, t: &Theme, cx: &mut Context<Self>) -> Div {
        let p = &v.procs;
        let gpu = p.gpu_known;
        let mut list = div().id("process-list").flex_1().min_h_0().overflow_y_scroll().flex().flex_col();

        // ---- why the fans came on
        list = list.child(section_title("Why the fans came on", t));
        if v.spins.is_empty() {
            list = list.child(
                div()
                    .mt(pt(4.))
                    .mb(pt(4.))
                    .text_size(pt(12.))
                    .text_color(t.text_2)
                    .child("The fans haven't come on since Mac Smart Cooler started. When they do, what was busy in the minute before is listed here."),
            );
        } else {
            let shown = if self.all_spins { v.spins.len() } else { 1 };
            for (i, e) in v.spins.iter().take(shown).enumerate() {
                list = list.child(spin_card(i, e, gpu, t));
            }
            if v.spins.len() > 1 {
                let all = self.all_spins;
                let label = if all { "Show only the latest".to_string() } else { format!("Show {} earlier", v.spins.len() - 1) };
                list = list.child(
                    div()
                        .id("all-spins")
                        .flex_none()
                        .mt(pt(-2.))
                        .mb(pt(4.))
                        .text_size(pt(12.))
                        .text_color(t.accent)
                        .cursor_pointer()
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.all_spins = !all;
                            cx.notify();
                        })),
                );
            }
        }

        // ---- right now
        let window = if p.span >= 1.0 && p.span < procs::TABLE_SECS - 1.0 { format!("last {:.0} s", p.span) } else { "last 30 s".into() };
        list = list.child(section_title(&format!("Right now · {window}"), t));
        let sort = self.sort;
        let header = |id: &'static str, label: &'static str, by: Sort, w: f32, hint: &'static str, cx: &mut Context<Self>| {
            let on = sort == by;
            div()
                .id(id)
                .w(pt(w))
                .flex_none()
                .flex()
                .items_center()
                .justify_end()
                .gap(pt(3.))
                .cursor_pointer()
                .text_color(if on { t.text_2 } else { t.text_3 })
                .hover(|s| s.text_color(t.text))
                .tooltip(tip(hint, t))
                .when(on, |el| el.child(icon("arrow-down", 8., t.text_2)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sort = by;
                    cx.notify();
                }))
        };
        list = list.child(
            div()
                .flex_none()
                .h(pt(22.))
                .pl(pt(17.))
                .pr(pt(6.))
                .flex()
                .items_center()
                .gap(pt(8.))
                .text_size(pt(10.5))
                .child(div().flex_1().text_color(t.text_3).child("PROCESS"))
                .child(header("sort-cpu", "CPU", Sort::Cpu, 52., "Averaged over the last 30 s. 100% is one core fully busy, as in Activity Monitor.", cx))
                .when(gpu, |el| el.child(header("sort-gpu", "GPU", Sort::Gpu, 44., "Share of the graphics processor, averaged over the last 30 s.", cx))),
        );

        if let Some(err) = &p.error {
            list = list.child(div().mt(pt(6.)).text_color(t.danger).child(format!("Can't read processes: {err}")));
        } else if p.span <= 0.0 {
            list = list.child(
                div().mt(pt(8.)).flex().items_center().gap(pt(8.)).child(spinner("procs-loading", 14., t)).child(div().text_color(t.text_2).child("Measuring…")),
            );
        } else {
            let mut rows: Vec<Usage> = p.table.clone();
            procs::sort_rows(&mut rows, sort);
            let n = rows.len();
            for (i, r) in rows.iter().enumerate() {
                let level = match sort {
                    Sort::Cpu => r.cpu.min(1.0),
                    Sort::Gpu => r.gpu,
                };
                list = list.child(
                    div()
                        .id(("proc", i))
                        .relative()
                        .flex_none()
                        .h(pt(28.))
                        .pl(pt(1.))
                        .pr(pt(6.))
                        .rounded(pt(6.))
                        .flex()
                        .items_center()
                        .gap(pt(10.))
                        .hover(|s| s.bg(t.hover))
                        .tooltip(tip(process_tip(r), t))
                        .child(div().size(pt(6.)).flex_none().rounded_full().bg(t.load(level)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap(pt(5.))
                                .child(div().flex_1().min_w_0().ellipsis().text_color(mix(t.text_2, t.text, 0.6)).child(elided(r.name.clone())))
                                .when(r.pids.len() > 1, |el| el.child(div().flex_none().text_size(pt(11.)).text_color(t.text_3).child(format!("×{}", r.pids.len())))),
                        )
                        .child(num(13.).w(pt(52.)).flex_none().flex().justify_end().text_color(if r.cpu >= 0.5 { t.load(r.cpu.min(1.0)) } else { t.text }).child(procs::cpu_text(r.cpu)))
                        .when(gpu, |el| el.child(num(13.).w(pt(44.)).flex_none().flex().justify_end().text_color(if r.gpu >= 0.25 { t.load(r.gpu) } else { t.text_2 }).child(procs::gpu_text(r.gpu))))
                        .when(i + 1 < n, |el| el.child(div().absolute().bottom_0().left(pt(16.)).right_0().h(pt(1.)).bg(t.border.opacity(0.6)))),
                );
            }
            if n == 0 {
                list = list.child(div().mt(pt(6.)).text_color(t.text_2).child("Nothing is busy."));
            }
        }
        div().flex_1().min_h_0().flex().flex_col().child(list)
    }

    /* ================================================================
       Inputs that need the view: sliders and steppers
       ================================================================ */

    /// A slider at `frac` (0..1). Dragging reports to `slide`; the track's bounds are
    /// recorded as it is drawn, and while dragging, window-wide mouse events follow the
    /// pointer even outside the track.
    fn slider(&mut self, id: SliderId, frac: f32, t: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let cell = self.tracks.entry(id).or_insert_with(|| Rc::new(Cell::new(Bounds::default()))).clone();
        let dragging = self.drag == Some(id);
        let me: WeakEntity<Self> = cx.entity().downgrade();
        div()
            .id(SharedString::from(format!("slider-{id:?}")))
            .relative()
            .w_full()
            .cursor_pointer()
            .child(slider_track(frac, t))
            .child(
                canvas(
                    move |bounds, _, _| cell.set(bounds),
                    move |_, _, window, _| {
                        if !dragging {
                            return;
                        }
                        let m = me.clone();
                        window.on_mouse_event(move |e: &MouseMoveEvent, _, _, cx| {
                            if e.pressed_button == Some(MouseButton::Left) {
                                m.update(cx, |this, cx| this.slide(id, e.position, cx)).ok();
                            }
                        });
                        let m = me.clone();
                        window.on_mouse_event(move |e: &MouseUpEvent, _, _, cx| {
                            if e.button == MouseButton::Left {
                                m.update(cx, |this, cx| {
                                    this.drag = None;
                                    cx.notify();
                                })
                                .ok();
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    this.drag = Some(id);
                    this.slide(id, e.position, cx);
                    cx.notify();
                }),
            )
    }

    /// "− 60 °C +" with press-and-hold repeat.
    fn stepper(&self, id: StepId, value: String, t: &Theme, cx: &mut Context<Self>) -> Div {
        let btn = |name: &'static str, icon_name: &'static str, dir: i32, cx: &mut Context<Self>| {
            step_button(SharedString::from(format!("{name}-{id:?}")), icon_name, true, t)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| this.start_hold(id, dir, cx)))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.hold = None))
                .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| this.hold = None))
        };
        segmented(t)
            .flex_none()
            .child(btn("dec", "minus", -1, cx))
            .child(
                div()
                    .w(pt(64.))
                    .h(pt(24.))
                    .rounded(pt(6.))
                    .bg(t.surface)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(pt(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(value),
            )
            .child(btn("inc", "plus", 1, cx))
    }

    /* ================================================================
       Dialogs
       ================================================================ */

    fn fan_dialog(&mut self, v: &V, t: &Theme, max_h: Pixels, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let d = self.dialog.as_ref()?;
        let fan = v.snap.fans.get(d.fan)?.clone();
        let (kind, rpm, sensor, lo, hi) = (d.kind, d.rpm, d.sensor.clone(), d.lo, d.hi);
        let kinds = segmented(t).children(["Fixed speed", "Follow a sensor"].into_iter().enumerate().map(|(i, label)| {
            let k = if i == 0 { DialogKind::Constant } else { DialogKind::Sensor };
            segment(("dlg-kind", i), label, kind == k, t).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(d) = &mut this.dialog {
                    d.kind = k;
                }
                this.popup = None;
                cx.notify();
            }))
        }));

        let body = if kind == DialogKind::Constant {
            let frac = ((rpm - fan.min) / (fan.max - fan.min)) as f32;
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(pt(6.))
                        .child(div().text_size(pt(30.)).font_weight(FontWeight::BOLD).line_height(pt(36.)).child(thousands(rpm)))
                        .child(div().text_color(t.text_3).child("rpm")),
                )
                .child(div().mt(pt(8.)).child(self.slider(SliderId::Rpm, frac, t, cx)))
                .child(
                    div()
                        .mt(pt(4.))
                        .flex()
                        .text_size(pt(11.))
                        .text_color(t.text_3)
                        .child(format!("min {}", thousands(fan.min)))
                        .child(div().flex_1())
                        .child(format!("max {}", thousands(fan.max))),
                )
        } else {
            let current = v
                .snap
                .sensors
                .iter()
                .find(|r| r.key == sensor)
                .map(|r| format!("{}  ({})", display_name(r), v.temp(r.value)))
                .unwrap_or_else(|| sensor.clone());
            let choices: Vec<(String, String)> = v
                .snap
                .sensors
                .iter()
                .filter(|r| r.named || v.s.show_unnamed)
                .map(|r| (r.key.clone(), format!("{}  ({})", display_name(r), v.temp(r.value))))
                .collect();
            let sel = sensor.clone();
            let picker = dropdown("sensor-pick", current, t).w_full().on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let items = choices
                        .iter()
                        .map(|(k, label)| PopItem { label: label.clone().into(), checked: *k == sel, cmd: Cmd::Sensor(k.clone()) })
                        .collect();
                    this.popup = Some(Popup { pos: e.position, width: 300., items });
                    cx.notify();
                }),
            );
            let row = |label: &'static str, id: StepId, value: f64, cx: &mut Context<Self>| {
                div()
                    .flex()
                    .items_center()
                    .gap(pt(12.))
                    .child(div().flex_1().text_color(t.text_2).child(label))
                    .child(self.stepper(id, v.temp(value), t, cx))
            };
            div()
                .flex()
                .flex_col()
                .gap(pt(12.))
                .child(div().flex().flex_col().gap(pt(6.)).child(div().text_color(t.text_2).child("Sensor")).child(picker))
                .child(row("Start speeding up at", StepId::Lo, lo, cx))
                .child(row("Full speed at", StepId::Hi, hi, cx))
                .child(div().text_size(pt(12.)).line_height(pt(17.)).text_color(t.text_3).child(format!(
                    "Below {} the fan runs at its minimum ({} rpm).",
                    v.temp(lo),
                    thousands(fan.min)
                )))
        };

        Some(
            modal_frame(t, pt(464.), max_h)
                .child(
                    div()
                        .p(pt(22.))
                        .pb(pt(10.))
                        .child(div().text_size(pt(18.)).child(format!("{} fan", fan.name)))
                        .child(div().text_size(pt(12.5)).text_color(t.text_2).child("Choose how this fan should run.")),
                )
                .child(div().id("fan-body").flex_1().min_h_0().overflow_y_scroll().px(pt(22.)).flex().flex_col().gap(pt(14.)).child(kinds).child(body).when(
                    !v.snap.can_control,
                    |el| {
                        el.child(
                            div()
                                .px(pt(12.))
                                .py(pt(10.))
                                .rounded(pt(10.))
                                .bg(t.warning_soft)
                                .flex()
                                .items_start()
                                .gap(pt(10.))
                                .child(div().mt(pt(1.)).child(icon("alert", 16., t.warning)))
                                .child(div().flex_1().min_w_0().line_height(pt(19.)).child("Turn on fan control first: click “Enable fan control” at the top.")),
                        )
                    },
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(pt(8.))
                        .p(pt(22.))
                        .pt(pt(16.))
                        .child(div().flex_1())
                        .child(button("dlg-cancel", "Cancel", None, Btn::Secondary, true, t).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))))
                        .child(button("dlg-apply", "Apply", None, Btn::Primary, true, t).on_click(cx.listener(|this, _, _, cx| this.apply_dialog(cx)))),
                ),
        )
    }

    fn prefs_dialog(&mut self, v: &V, t: &Theme, max_h: Pixels, cx: &mut Context<Self>) -> Stateful<Div> {
        let row = |label: &'static str, control: AnyDiv| {
            div().min_h(pt(32.)).flex().items_center().gap(pt(16.)).child(div().flex_1().min_w_0().text_color(t.text_2).child(label)).child(control.0)
        };
        let unit = segmented(t).w(pt(110.)).children([("°C", false), ("°F", true)].into_iter().enumerate().map(|(i, (label, f))| {
            segment(("unit", i), label, v.s.fahrenheit == f, t).on_click(cx.listener(move |this, _, _, cx| this.settings(cx, |s| s.fahrenheit = f)))
        }));
        let theme = segmented(t).w(pt(186.)).children(
            [(ThemeMode::Light, "Light"), (ThemeMode::Dark, "Dark"), (ThemeMode::System, "Auto")].into_iter().enumerate().map(|(i, (mode, label))| {
                segment(("appearance", i), label, self.prefs.theme == mode, t).on_click(cx.listener(move |this, _, _, cx| this.set_theme(mode, cx)))
            }),
        );
        let show_unnamed = v.s.show_unnamed;
        let unnamed = switch("unnamed", show_unnamed, t).on_click(cx.listener(move |this, _, _, cx| this.settings(cx, |s| s.show_unnamed = !show_unnamed)));
        let curve = |this: &mut Self, label: &'static str, id: SliderId, value: f64, frac: f32, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .gap(pt(16.))
                .min_h(pt(32.))
                .child(div().w(pt(122.)).flex_none().text_color(t.text_2).child(label))
                .child(div().flex_1().child(this.slider(id, frac, t, cx)))
                .child(div().w(pt(52.)).flex_none().text_right().font_weight(FontWeight::SEMIBOLD).child(v.temp(value)))
        };
        let lo = curve(self, "Cooling starts at", SliderId::SmartLo, v.s.smart_lo, ((v.s.smart_lo - 45.) / 40.) as f32, cx);
        let hi = curve(self, "Full speed at", SliderId::SmartHi, v.s.smart_hi, ((v.s.smart_hi - 60.) / 40.) as f32, cx);
        let text = |s: &'static str| div().text_size(pt(12.)).line_height(pt(17.)).text_color(t.text_2).child(s);
        let heading = |s: &'static str| div().mt(pt(14.)).mb(pt(2.)).text_size(pt(10.5)).text_color(t.text_3).child(s);

        modal_frame(t, pt(424.), max_h)
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(pt(22.))
                    .pt(pt(20.))
                    .child(div().flex_1().text_size(pt(18.)).child("Preferences"))
                    .child(button("prefs-done", "Done", None, Btn::Secondary, true, t).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx)))),
            )
            .child(
                div()
                    .id("prefs-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(pt(22.))
                    .pb(pt(22.))
                    .flex()
                    .flex_col()
                    .child(heading("DISPLAY"))
                    .child(row("Temperature unit", AnyDiv(unit)))
                    .child(row("Appearance", AnyDiv(theme)))
                    .child(row("Show unidentified SMC sensors", AnyDiv(div().child(unnamed))))
                    .child(heading("SMART COOLING"))
                    .child(text(
                        "Fans stay with macOS until the heat is real: warm air in the back of the case or a warm palm rest \
                         while the Mac is busy, or minutes of heavy work with a hot chip. Launches and brief spikes never spin \
                         them up. The temperatures below are for the chip.",
                    ))
                    .child(div().mt(pt(6.)).child(lo))
                    .child(hi)
                    .child(heading("BACKGROUND HELPER"))
                    .child(text(
                        "Your mode keeps running after you quit Mac Smart Cooler and after restarts. Choose Apple Default to \
                         give the fans back to macOS.",
                    ))
                    .when(v.installing, |el| {
                        el.child(div().mt(pt(10.)).flex().items_center().gap(pt(8.)).child(spinner("prefs-wait", 14., t)).child(div().text_color(t.text_2).child("Waiting for your password…")))
                    })
                    .when(!v.installing && v.helper != helper::Status::NotInstalled, |el| {
                        el.child(div().mt(pt(10.)).flex().child(
                            button("remove-helper", "Remove background helper", None, Btn::Secondary, true, t)
                                .on_click(cx.listener(|this, _, _, cx| this.ctl.read(cx).request_uninstall())),
                        ))
                    }),
            )
    }

    fn about(&self, t: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        modal_frame(t, pt(360.), pt(440.)).child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .px(pt(28.))
                .pt(pt(24.))
                .pb(pt(24.))
                .child(gpui::img(self.app_icon.clone()).size(pt(96.)))
                .child(div().mt(pt(4.)).text_size(pt(20.)).font_weight(FontWeight::BOLD).child("Mac Smart Cooler"))
                .child(div().mt(pt(2.)).text_color(t.text_3).child(format!("Version {}", env!("CARGO_PKG_VERSION"))))
                .child(
                    div()
                        .mt(pt(14.))
                        .mb(pt(20.))
                        .text_center()
                        .line_height(pt(19.))
                        .text_color(t.text_2)
                        .child("Fan control and temperatures for your Mac. Smart Cooling keeps the fans quiet until the heat is real."),
                )
                .child(button("about-close", "Close", None, Btn::Secondary, true, t).on_click(cx.listener(|this, _, _, cx| this.close_modal(cx)))),
        )
    }

    /// The open popup menu on a click-catching layer that closes it. Long menus scroll.
    fn popup_layer(&self, t: &Theme, viewport: Size<Pixels>, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let popup = self.popup.as_ref()?;
        let zoom = self.prefs.zoom;
        let any_checked = popup.items.iter().any(|i| i.checked);
        // Placed at a mouse position, so these are real pixels at the current zoom.
        let width = gpui::px(popup.width * zoom);
        let margin = gpui::px(8. * zoom);
        let height = gpui::px((10. + 28. * popup.items.len() as f32) * zoom).min(viewport.height - margin * 2.);
        let x = popup.pos.x.min(viewport.width - width - margin).max(margin);
        let y = popup.pos.y.min(viewport.height - height - margin).max(margin);

        let items = popup.items.iter().enumerate().map(|(i, item)| {
            let cmd = item.cmd.clone();
            div()
                .id(("popup-item", i))
                .h(pt(28.))
                .flex_none()
                .px(pt(8.))
                .rounded(pt(6.))
                .flex()
                .items_center()
                .gap(pt(6.))
                .cursor_pointer()
                .hover(|s| s.bg(t.accent_soft))
                .when(any_checked, |el| {
                    el.child(div().w(pt(14.)).flex_none().when(item.checked, |d| d.child(icon("check-bold", 11., t.accent))))
                })
                .child(div().flex_1().min_w_0().ellipsis().child(elided(item.label.clone())))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.popup = None;
                    cx.notify();
                    this.run_cmd(cmd.clone(), cx);
                }))
        });

        Some(
            div()
                .id("popup-layer")
                .absolute()
                .inset_0()
                .occlude()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.popup = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .id("popup")
                        .absolute()
                        .left(x)
                        .top(y)
                        .w(width)
                        .h(height)
                        .p(pt(4.))
                        .rounded(pt(10.))
                        .bg(t.surface)
                        .border_1()
                        .border_color(t.border_strong)
                        .shadow(vec![shadow(rgba_hsla(0x00000030), 8., 24., t.scale)])
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .children(items),
                ),
        )
    }
}

/// A Div passed by value into a row builder.
struct AnyDiv(Div);

impl Render for Main {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut t = match self.prefs.theme {
            ThemeMode::Light => Theme::light(),
            ThemeMode::Dark => Theme::dark(),
            ThemeMode::System => Theme::for_appearance(window.appearance()),
        };
        t.scale = self.prefs.zoom;
        // Keyboard shortcuts and menu items reach this view through its focus.
        if !self.focus.is_focused(window) {
            window.focus(&self.focus);
        }
        let v = V::of(self.ctl.read(cx));
        if self.prefs.processes {
            self.ctl.read(cx).watch_procs();
        }
        // the fan the dialog was for is gone (or never had a usable range)
        if self.modal == Modal::Fan && self.dialog.as_ref().is_none_or(|d| v.snap.fans.get(d.fan).is_none_or(|f| !(f.max > f.min))) {
            self.modal = Modal::None;
            self.dialog = None;
        }
        let viewport = window.viewport_size();
        let max_h = (viewport.height - gpui::px(64. * self.prefs.zoom)).min(gpui::px(640. * self.prefs.zoom));
        let modal = match self.modal {
            Modal::None => None,
            Modal::Fan => self.fan_dialog(&v, &t, max_h, cx),
            Modal::Prefs => Some(self.prefs_dialog(&v, &t, max_h, cx)),
            Modal::About => Some(self.about(&t, cx)),
        };
        let popup = self.popup_layer(&t, viewport, cx);

        div()
            .key_context("MacSmartCooler")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Cancel, _, cx| this.on_cancel(cx)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| {
                if this.modal == Modal::Fan && this.popup.is_none() {
                    this.apply_dialog(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &About, _, cx| {
                this.close_modal(cx);
                this.modal = Modal::About;
            }))
            .on_action(cx.listener(|this, _: &Preferences, _, cx| {
                this.close_modal(cx);
                this.modal = Modal::Prefs;
            }))
            .on_action(cx.listener(|_, _: &CloseWindow, window, _| window.remove_window()))
            .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|this, _: &ThemeLight, _, cx| this.set_theme(ThemeMode::Light, cx)))
            .on_action(cx.listener(|this, _: &ThemeDark, _, cx| this.set_theme(ThemeMode::Dark, cx)))
            .on_action(cx.listener(|this, _: &ThemeAuto, _, cx| this.set_theme(ThemeMode::System, cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, window, cx| this.step_zoom(1, window, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, window, cx| this.step_zoom(-1, window, cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, window, cx| this.set_zoom(1., window, cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(linear_gradient(180., linear_color_stop(t.bg, 0.), linear_color_stop(t.bg_bottom, 1.)))
            .text_color(t.text)
            .font_family(".SystemUIFont")
            .text_size(pt(13.5))
            .line_height(relative(1.3))
            .child(self.titlebar(&v, &t, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .gap(GAP)
                            .pl(pt(20.))
                            .pr(GAP)
                            .pt(pt(6.))
                            .pb(pt(20.))
                            .child(self.hero(&v, &t, cx))
                            .child(self.mode_cards(&v, &t, cx))
                            .child(self.fans(&v, &t, cx))
                            .when_some(v.snap.error.clone(), |el, e| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .px(pt(14.))
                                        .py(pt(9.))
                                        .rounded(pt(10.))
                                        .bg(t.danger.opacity(0.10))
                                        .border_1()
                                        .border_color(t.danger.opacity(0.24))
                                        .text_size(pt(12.5))
                                        .text_color(mix(t.danger, t.text, 0.35))
                                        .child(e),
                                )
                            }),
                    )
                    .child(self.sidebar(&v, &t, cx)),
            )
            .when_some(modal, |el, card| {
                // Like the original: dialogs float over the window without dimming it.
                // Preferences hangs under the gear; the others are centred.
                let prefs = self.modal == Modal::Prefs;
                el.child(
                    div()
                        .id("backdrop")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .flex()
                        .when(prefs, |el| el.justify_end().items_start().pt(pt(52.)).pr(pt(20.)))
                        .when(!prefs, |el| el.items_center().justify_center().p(pt(32.)))
                        .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx)))
                        .child(card),
                )
            })
            .when_some(popup, |el, layer| el.child(layer))
    }
}

/// SF Pro Rounded, for numbers (loaded at startup in main.rs).
const ROUNDED: &str = ".SF NS Rounded";

fn num(size: f32) -> Div {
    div().font_family(ROUNDED).text_size(pt(size))
}

/// The original card: flat, with a hairline edge (a soft shadow in light mode).
fn panel(t: &Theme) -> Div {
    div()
        .rounded(CARD_RADIUS)
        .bg(t.surface)
        .border_1()
        .border_color(t.border)
        .when(!t.dark, |el| el.shadow(small_shadow(t)))
}

/// A small upper-case heading inside a sidebar list.
fn section_title(text: &str, t: &Theme) -> Div {
    div().flex_none().mt(pt(14.)).mb(pt(4.)).text_size(pt(10.5)).text_color(t.text_3).child(text.to_uppercase())
}

/// "Blender · pid 4120 · your account", or the pids of every process with that name.
fn process_tip(r: &Usage) -> String {
    let pids = match r.pids.as_slice() {
        [one] => format!("pid {one}"),
        many => {
            let list: Vec<String> = many.iter().take(8).map(|p| p.to_string()).collect();
            let more = if many.len() > 8 { format!(" and {} more", many.len() - 8) } else { String::new() };
            format!("{} processes: pids {}{more}", many.len(), list.join(", "))
        }
    };
    let owner = match r.owner {
        Owner::You => "runs as you",
        Owner::System => "a macOS system process",
        Owner::OtherUser => "runs as another account",
    };
    match procs::explain(&r.name) {
        Some(what) => format!("{}\n{what}.\n{pids} · {owner}", r.name),
        None => format!("{}\n{pids} · {owner}", r.name),
    }
}

/// One time the fans came on: when, how fast, for how long, and the busiest processes.
fn spin_card(i: usize, e: &FanEvent, gpu: bool, t: &Theme) -> Stateful<Div> {
    let when = std::time::SystemTime::now().duration_since(e.wall).unwrap_or_default();
    let status = match e.lasted {
        None => "still on".to_string(),
        Some(d) => format!("on for {}", procs::duration_text(d)),
    };
    let title = if e.at_launch {
        match e.lasted {
            None => "On since the app started".to_string(),
            Some(d) => format!("On at launch, for {}", procs::duration_text(d)),
        }
    } else {
        format!("{} · {status}", capitalize(&procs::ago(when)))
    };
    let culprits = e.culprits.iter().enumerate().map(|(j, u)| {
        let mut load = format!("{} CPU", procs::cpu_text(u.cpu));
        if gpu && u.gpu >= 0.05 {
            load += &format!(" · {} GPU", procs::gpu_text(u.gpu));
        }
        div()
            .id(("culprit", i * 10 + j))
            .flex()
            .items_center()
            .gap(pt(8.))
            .h(pt(20.))
            .tooltip(tip(process_tip(u), t))
            .child(div().flex_1().min_w_0().ellipsis().child(elided(u.name.clone())))
            .child(num(12.).flex_none().text_color(t.text_2).child(load))
    });
    div()
        .id(("spin", i))
        .flex_none()
        .mb(pt(8.))
        .px(pt(12.))
        .py(pt(10.))
        .rounded(pt(10.))
        .bg(t.surface_2)
        .border_1()
        .border_color(t.border)
        .flex()
        .flex_col()
        .gap(pt(2.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(pt(6.))
                .child(icon("fan", 13., if e.lasted.is_none() { t.cyan } else { t.text_3 }))
                .child(div().flex_1().min_w_0().ellipsis().text_size(pt(13.)).child(elided(title)))
                .child(num(11.5).flex_none().text_color(t.text_3).child(format!("{} rpm", thousands(e.peak_rpm)))),
        )
        .when_some(e.why.clone(), |el, why| el.child(div().text_size(pt(11.5)).text_color(t.text_2).child(format!("Smart Cooling: {why}"))))
        .child(div().mt(pt(4.)).text_size(pt(10.)).text_color(t.text_3).child(if e.at_launch { "BUSIEST SINCE THE APP STARTED" } else { "BUSIEST IN THE MINUTE BEFORE" }))
        .when(e.culprits.is_empty(), |el| {
            let note = if e.at_launch { "Measuring…" } else { "Nothing stood out (the Mac was mostly idle)." };
            el.child(div().text_size(pt(12.)).text_color(t.text_2).child(note))
        })
        .child(div().flex().flex_col().text_size(pt(12.5)).children(culprits))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// A button that opens a popup menu (Mac Cleaner's dropdown).
fn dropdown(id: &'static str, label: String, t: &Theme) -> Stateful<Div> {
    let hover = t.surface_2;
    div()
        .id(id)
        .h(pt(32.))
        .px(pt(10.))
        .rounded(pt(8.))
        .bg(t.surface)
        .border_1()
        .border_color(t.border_strong)
        .flex()
        .items_center()
        .gap(pt(6.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .whitespace_nowrap()
        .text_size(pt(12.5))
        .child(div().flex_1().min_w_0().ellipsis().child(elided(label)))
        .child(icon("chevron-down", 10., t.text_3))
}

/// A window size given in points at 100% zoom, scaled and kept inside the screen.
pub fn window_size(points: (f32, f32), zoom: f32) -> Size<Pixels> {
    let (max_w, max_h) = crate::native::screen_visible_size().unwrap_or((f32::MAX, f32::MAX));
    gpui::size(gpui::px((points.0 * zoom).min(max_w)), gpui::px((points.1 * zoom).min(max_h)))
}

/* ================================================================
   Snapshot walkthrough (debug builds only)
   ================================================================ */

#[cfg(debug_assertions)]
impl Main {
    /// Saves a PNG of each screen to `dir`, then quits. Never changes fans or saved settings:
    /// `Ctl` is frozen (no commands, no prompts, no saving) and the states shown are display-only.
    pub fn start_demo(&mut self, dir: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wid) = crate::snapshot::window_number(window) else { return };
        self.prefs.frozen = true;
        self.prefs.processes = false; // the walkthrough shows the sensors first, whatever was saved
        let snap = move |name: &str| {
            let path = format!("{dir}/{name}.png");
            eprintln!("snapshot {path}: {}", crate::snapshot::save(wid, &path));
        };
        cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let wait = move |ms| executor.timer(Duration::from_millis(ms));
            // A covered window is not redrawn: bring it forward (and make it key, so the
            // title-bar buttons are in colour) before every picture.
            let shot = |this: &WeakEntity<Self>, cx: &mut gpui::AsyncWindowContext| {
                this.update_in(cx, |_, window, cx| {
                    crate::native::activate(window);
                    cx.notify();
                })
                .ok();
            };
            let set = |this: &WeakEntity<Self>, cx: &mut gpui::AsyncWindowContext, f: Box<dyn FnOnce(&mut Self, &mut Context<Self>)>| {
                this.update(cx, |this, cx| {
                    f(this, cx);
                    cx.notify();
                })
                .ok();
            };
            let demo = |this: &WeakEntity<Self>, cx: &mut gpui::AsyncWindowContext, d: crate::state::Demo| {
                this.update(cx, |this, cx| this.ctl.update(cx, |c, cx| {
                    c.demo = d;
                    c.tick(cx);
                }))
                .ok();
            };
            // wait for real readings (or the sample) and two seconds of history
            wait(2500).await;
            shot(&this, cx);
            wait(400).await;
            snap("1-main");

            set(&this, cx, Box::new(|this, cx| {
                let c = this.ctl.read(cx);
                if let Some(f) = c.snap.fans.first().cloned() {
                    this.open_dialog(&f, &FanMode::Constant(3400.), cx);
                }
            }));
            shot(&this, cx);
            wait(500).await;
            snap("2-fan-fixed");
            set(&this, cx, Box::new(|this, _| {
                if let Some(d) = &mut this.dialog {
                    d.kind = DialogKind::Sensor;
                }
            }));
            shot(&this, cx);
            wait(500).await;
            snap("3-fan-sensor");
            set(&this, cx, Box::new(|this, cx| {
                let v = V::of(this.ctl.read(cx));
                let items = v
                    .snap
                    .sensors
                    .iter()
                    .filter(|r| r.named)
                    .map(|r| PopItem { label: format!("{}  ({})", display_name(r), v.temp(r.value)).into(), checked: r.key == HOT_KEY, cmd: Cmd::Sensor(r.key.clone()) })
                    .collect();
                let z = this.prefs.zoom;
                this.popup = Some(Popup { pos: point(gpui::px(420. * z), gpui::px(330. * z)), width: 300., items });
            }));
            shot(&this, cx);
            wait(500).await;
            snap("4-sensor-menu");

            set(&this, cx, Box::new(|this, cx| {
                this.close_modal(cx);
                this.modal = Modal::Prefs;
            }));
            shot(&this, cx);
            wait(500).await;
            snap("5-preferences");
            set(&this, cx, Box::new(|this, cx| {
                this.close_modal(cx);
                this.modal = Modal::About;
            }));
            shot(&this, cx);
            wait(500).await;
            snap("6-about");
            set(&this, cx, Box::new(|this, cx| this.close_modal(cx)));
            set(&this, cx, Box::new(|this, _| this.prefs.processes = true));
            shot(&this, cx);
            wait(500).await;
            snap("6b-processes");
            set(&this, cx, Box::new(|this, _| this.prefs.processes = false));

            demo(&this, cx, crate::state::Demo { no_helper: true, error: true, preset: Some(Preset::Automatic), ..Default::default() });
            shot(&this, cx);
            wait(500).await;
            snap("7-no-helper-error");
            demo(&this, cx, crate::state::Demo { outdated: true, preset: Some(Preset::FullBlast), ..Default::default() });
            shot(&this, cx);
            wait(500).await;
            snap("8-outdated-fullblast");
            demo(&this, cx, crate::state::Demo { installing: true, preset: Some(Preset::Custom), ..Default::default() });
            shot(&this, cx);
            wait(500).await;
            snap("9-installing-custom");
            demo(&this, cx, crate::state::Demo::default());

            this.update_in(cx, |this, window, _| window.resize(window_size(WINDOW_MIN, this.prefs.zoom))).ok();
            wait(900).await;
            shot(&this, cx);
            wait(400).await;
            snap("10-min-size");
            this.update(cx, |_, cx| cx.quit()).ok();
        })
        .detach();
    }
}
