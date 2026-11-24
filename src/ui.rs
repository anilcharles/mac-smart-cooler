//! egui front-end.
//!
//! Visual language: a dark, layered window with a transparent title bar; soft cards with
//! hairline borders and 16 px corners; SF Pro for text and SF Pro Rounded for numbers;
//! one cool accent (blue/cyan) and a continuous teal -> amber -> red temperature scale.

use crate::fans::FanInfo;
use crate::helper;
use crate::sensors::{Reading, HOT_KEY};
use crate::settings::{self, Settings};
use crate::smc::is_root;
use crate::worker::{Cmd, FanMode, Snapshot, Worker};
use eframe::egui::{
    self, epaint::Shadow, Align, Align2, Color32, FontFamily, FontId, Id, Layout, Margin, Pos2,
    Rect, Response, RichText, Rounding, Sense, Stroke, Vec2,
};
use std::f32::consts::{PI, TAU};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

// ---------------------------------------------------------------- palette & metrics

const BG_TOP: Color32 = Color32::from_rgb(19, 20, 25);
const BG_BOTTOM: Color32 = Color32::from_rgb(11, 12, 15);
const CARD: Color32 = Color32::from_rgb(25, 27, 33);
const RAISED: Color32 = Color32::from_rgb(34, 37, 44);
const TRACK: Color32 = Color32::from_rgb(44, 47, 55);
const TEXT: Color32 = Color32::from_rgb(242, 243, 246);
const DIM: Color32 = Color32::from_rgb(152, 157, 168);
const FAINT: Color32 = Color32::from_rgb(98, 103, 114);
const ACCENT: Color32 = Color32::from_rgb(10, 132, 255);
const CYAN: Color32 = Color32::from_rgb(100, 210, 255);
const COOL: Color32 = Color32::from_rgb(48, 209, 160);
const WARM: Color32 = Color32::from_rgb(255, 196, 64);
const HOT: Color32 = Color32::from_rgb(255, 128, 64);
const RED: Color32 = Color32::from_rgb(255, 69, 58);
const GREEN: Color32 = Color32::from_rgb(48, 209, 88);
const AMBER: Color32 = Color32::from_rgb(255, 196, 64);

fn line() -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, 16)
}

const RADIUS: f32 = 10.0;
const CARD_RADIUS: f32 = 16.0;
const GAP: f32 = 14.0;
const TITLEBAR: f32 = 30.0;
const TEXT_SIZE: f32 = 13.5;
const HIST_SECS: usize = 120;

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

#[derive(PartialEq, Clone, Copy, Debug)]
enum Preset {
    Automatic,
    Smart,
    FullBlast,
    Custom,
}

impl Preset {
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
}

static INSTALLING: AtomicBool = AtomicBool::new(false);

/// Install (or update) the background helper, off the UI thread. One password prompt.
fn request_install(ctx: egui::Context) {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let _ = helper::install();
        // give launchd a moment to start it, then let the UI refresh its status
        std::thread::sleep(Duration::from_millis(1500));
        INSTALLING.store(false, Ordering::SeqCst);
        ctx.request_repaint();
    });
}

fn request_uninstall(ctx: egui::Context) {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let _ = helper::uninstall();
        INSTALLING.store(false, Ordering::SeqCst);
        ctx.request_repaint();
    });
}

pub struct App {
    helper: helper::Status,
    helper_checked: Option<std::time::Instant>,
    worker: Worker,
    dialog: Option<CustomDialog>,
    show_prefs: bool,
    s: Settings,
    saved: Settings,
    pending: Option<Preset>,
    /// settings handed to the helper since it became reachable (it keeps the system copy)
    helper_synced: bool,
    /// the preset just clicked, shown right away until the fans report it (or 4 s pass)
    chosen: Option<(Preset, std::time::Instant)>,
    /// asked once this launch to replace an outdated helper (so the latest cooling logic runs)
    auto_updated: bool,
    model: String,
    /// smoothed rpm per fan, as displayed
    rpm: Vec<Option<f32>>,
    /// last ~2 minutes of each fan's rpm, one sample per second
    hist: Vec<std::collections::VecDeque<f32>>,
    hist_at: Option<std::time::Instant>,
    /// menu bar item (none in screenshot mode)
    tray: Option<crate::tray::Tray>,
    /// Quit was chosen: let the window close for real instead of hiding to the menu bar
    quitting: bool,
    /// MACSMARTCOOLER_SHOT=path: render, save a PNG of the window, quit (for design review)
    shot: Option<(String, std::time::Instant, bool)>,
}

// ---------------------------------------------------------------- small helpers

fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

/// SF Pro Rounded, for numbers.
fn num(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("rounded".into()))
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

fn tint(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

/// Continuous temperature scale: teal (cool) -> amber -> orange -> red.
fn temp_color(c: f64) -> Color32 {
    let c = c as f32;
    if c < 45.0 {
        COOL
    } else if c < 65.0 {
        mix(COOL, WARM, (c - 45.0) / 20.0)
    } else if c < 82.0 {
        mix(WARM, HOT, (c - 65.0) / 17.0)
    } else {
        mix(HOT, RED, (c - 82.0) / 13.0)
    }
}

fn load_color(f: f64) -> Color32 {
    temp_color(40.0 + f * 45.0)
}

fn thousands(v: f64) -> String {
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

fn card_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, line()))
        .rounding(Rounding::same(CARD_RADIUS))
        .inner_margin(Margin::same(18.0))
}

/// Use the macOS system fonts: SF Pro for text, SF Pro Rounded for numbers.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |name: &str, paths: &[&str]| -> bool {
        for p in paths {
            if let Ok(bytes) = std::fs::read(p) {
                fonts
                    .font_data
                    .insert(name.into(), egui::FontData::from_owned(bytes));
                return true;
            }
        }
        false
    };
    let system = add(
        "system",
        &["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/HelveticaNeue.ttc"],
    );
    let rounded = add("rounded", &["/System/Library/Fonts/SFNSRounded.ttf"]);
    if system {
        if let Some(f) = fonts.families.get_mut(&FontFamily::Proportional) {
            f.insert(0, "system".into());
        }
    }
    let mut r = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    if rounded {
        r.insert(0, "rounded".into());
    }
    fonts.families.insert(FontFamily::Name("rounded".into()), r);
    ctx.set_fonts(fonts);
}

/// Rounded button. `primary` = filled accent, otherwise a quiet raised surface.
fn button(ui: &mut egui::Ui, text: &str, primary: bool) -> Response {
    let fg = if primary { Color32::WHITE } else { TEXT };
    let w = ui
        .painter()
        .layout_no_wrap(text.to_string(), font(13.0), fg)
        .size()
        .x
        + 28.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w.max(72.0), 28.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let base = if primary { ACCENT } else { RAISED };
    let fill = mix(base, Color32::WHITE, t * 0.08 + if resp.is_pointer_button_down_on() { 0.1 } else { 0.0 });
    ui.painter().rect(rect, Rounding::same(8.0), fill, if primary { Stroke::NONE } else { Stroke::new(1.0_f32, line()) });
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, text, font(13.0), fg);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Small rounded status pill with a dot.
fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    let w = ui
        .painter()
        .layout_no_wrap(text.to_string(), font(12.0), color)
        .size()
        .x
        + 30.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, Rounding::same(12.0), tint(color, 28));
    ui.painter()
        .circle_filled(rect.left_center() + Vec2::new(12.0, 0.0), 3.5, color);
    ui.painter().text(
        rect.left_center() + Vec2::new(21.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        font(12.0),
        color,
    );
}

/// Round icon button with a gear glyph.
fn gear_button(ui: &mut egui::Ui) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let p = ui.painter();
    p.circle(rect.center(), 14.0, mix(RAISED, Color32::WHITE, t * 0.08), Stroke::new(1.0_f32, line()));
    let c = rect.center();
    let col = mix(DIM, TEXT, t);
    for k in 0..8 {
        let a = k as f32 * TAU / 8.0;
        let d = Vec2::new(a.cos(), a.sin());
        p.line_segment([c + d * 4.5, c + d * 7.5], Stroke::new(2.6_f32, col));
    }
    p.circle_stroke(c, 4.6, Stroke::new(2.0_f32, col));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Preferences")
}

/// Compact segmented control with an animated sliding highlight. Returns the clicked index.
fn segmented(ui: &mut egui::Ui, width: f32, id: Id, options: &[&str], selected: Option<usize>) -> Option<usize> {
    let h = 26.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(rect, Rounding::same(8.0), Color32::from_rgb(17, 18, 22));
    let w = rect.width() / options.len() as f32;
    if let Some(sel) = selected {
        let x = ui.ctx().animate_value_with_time(id.with("x"), sel as f32, 0.18);
        let r = Rect::from_min_size(rect.min + Vec2::new(w * x, 0.0), Vec2::new(w, h)).shrink(2.0);
        p.rect_filled(r, Rounding::same(6.5), RAISED);
    }
    let mut clicked = None;
    for (i, label) in options.iter().enumerate() {
        let r = Rect::from_min_size(rect.min + Vec2::new(w * i as f32, 0.0), Vec2::new(w, h));
        let resp = ui.interact(r, id.with(i), Sense::click());
        let sel = selected == Some(i);
        let col = if sel { TEXT } else if resp.hovered() { mix(DIM, TEXT, 0.5) } else { DIM };
        p.text(r.center(), Align2::CENTER_CENTER, *label, font(12.0), col);
        if resp.clicked() {
            clicked = Some(i);
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    clicked
}

/// Stroke an arc (screen angles, clockwise) with round caps, coloured per segment.
fn arc(p: &egui::Painter, c: Pos2, r: f32, a0: f32, a1: f32, width: f32, color: impl Fn(f32) -> Color32) {
    let steps = (((a1 - a0).abs() / TAU) * 120.0).ceil().max(2.0) as usize;
    let at = |a: f32| c + Vec2::new(a.cos(), a.sin()) * r;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        p.line_segment(
            [at(a0 + (a1 - a0) * t0), at(a0 + (a1 - a0) * t1)],
            Stroke::new(width, color((t0 + t1) / 2.0)),
        );
    }
    p.circle_filled(at(a0), width / 2.0, color(0.0));
    p.circle_filled(at(a1), width / 2.0, color(1.0));
}

/// Horizontal meter with rounded ends; `frac` 0..1.
fn meter(p: &egui::Painter, r: Rect, frac: f32, from: Color32, to: Color32) {
    let rr = Rounding::same(r.height() / 2.0);
    p.rect_filled(r, rr, TRACK);
    let frac = frac.clamp(0.0, 1.0);
    if frac <= 0.0 {
        return;
    }
    let w = (r.width() * frac).max(r.height());
    let fill = Rect::from_min_size(r.min, Vec2::new(w, r.height()));
    // gradient via a vertex-coloured mesh, clipped to a rounded shape by drawing caps
    let mut mesh = egui::Mesh::default();
    let end = mix(from, to, frac);
    let (h, x0, x1) = (r.height() / 2.0, fill.left() + r.height() / 2.0, fill.right() - r.height() / 2.0);
    let x1 = x1.max(x0);
    mesh.colored_vertex(Pos2::new(x0, fill.top()), from);
    mesh.colored_vertex(Pos2::new(x1, fill.top()), end);
    mesh.colored_vertex(Pos2::new(x1, fill.bottom()), end);
    mesh.colored_vertex(Pos2::new(x0, fill.bottom()), from);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(mesh));
    p.circle_filled(Pos2::new(x0, fill.center().y), h, from);
    p.circle_filled(Pos2::new(x1, fill.center().y), h, end);
}

/// Area chart of recent values, newest on the right, scaled 0..max.
fn sparkline(p: &egui::Painter, r: Rect, v: &std::collections::VecDeque<f32>, max: f32) {
    // faint guide lines
    for k in 1..4 {
        let y = r.top() + r.height() * k as f32 / 4.0;
        p.line_segment([Pos2::new(r.left(), y), Pos2::new(r.right(), y)], Stroke::new(1.0_f32, tint(Color32::WHITE, 6)));
    }
    let n = HIST_SECS as f32 - 1.0;
    let start = HIST_SECS - v.len();
    let pts: Vec<Pos2> = v
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            Pos2::new(
                r.left() + r.width() * (start + i) as f32 / n,
                r.bottom() - r.height() * (x / max).clamp(0.0, 1.0),
            )
        })
        .collect();
    let mut mesh = egui::Mesh::default();
    let top = tint(ACCENT, 70);
    let bottom = tint(ACCENT, 0);
    for (i, q) in pts.iter().enumerate() {
        mesh.colored_vertex(*q, top);
        mesh.colored_vertex(Pos2::new(q.x, r.bottom()), bottom);
        if i > 0 {
            let b = (i as u32) * 2;
            mesh.add_triangle(b - 2, b - 1, b);
            mesh.add_triangle(b - 1, b + 1, b);
        }
    }
    p.add(egui::Shape::mesh(mesh));
    p.add(egui::Shape::line(pts.clone(), Stroke::new(1.6_f32, CYAN)));
    if let Some(last) = pts.last().filter(|_| v.back().is_some_and(|&x| x > 0.0)) {
        p.circle_filled(*last, 3.0, CYAN);
        p.circle_stroke(*last, 6.0, Stroke::new(1.0_f32, tint(CYAN, 70)));
    }
}

/// One line of text cut to `max_w` with an ellipsis.
fn elided(p: &egui::Painter, text: &str, font: FontId, color: Color32, max_w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: max_w,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    p.layout_job(job)
}

/// iOS-style switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(38.0, 22.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.15);
    let p = ui.painter();
    p.rect_filled(rect, Rounding::same(11.0), mix(TRACK, ACCENT, t));
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    p.circle_filled(Pos2::new(x, rect.center().y), 9.0, Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn c_to_f(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

/// Slider over a degC value that displays (and accepts typed values) in the chosen unit.
fn unit_fmt(s: egui::Slider<'_>, fahrenheit: bool) -> egui::Slider<'_> {
    if fahrenheit {
        s.custom_formatter(|v, _| format!("{:.0} °F", c_to_f(v)))
            .custom_parser(|t| t.trim().trim_end_matches("°F").trim().parse::<f64>().ok().map(|f| (f - 32.0) * 5.0 / 9.0))
    } else {
        s.custom_formatter(|v, _| format!("{v:.0} °C"))
            .custom_parser(|t| t.trim().trim_end_matches("°C").trim().parse::<f64>().ok())
    }
}

fn unit_drag(d: egui::DragValue<'_>, fahrenheit: bool) -> egui::DragValue<'_> {
    if fahrenheit {
        d.custom_formatter(|v, _| format!("{:.0} °F", c_to_f(v)))
            .custom_parser(|t| t.trim().trim_end_matches("°F").trim().parse::<f64>().ok().map(|f| (f - 32.0) * 5.0 / 9.0))
    } else {
        d.custom_formatter(|v, _| format!("{v:.0} °C"))
            .custom_parser(|t| t.trim().trim_end_matches("°C").trim().parse::<f64>().ok())
    }
}

/// Thin-line fan glyph.
fn paint_fan(p: &egui::Painter, c: Pos2, r: f32, angle: f32, spinning: bool) {
    let blade = if spinning { CYAN } else { FAINT };
    p.circle_filled(c, r, Color32::from_rgb(16, 17, 21));
    p.circle_stroke(c, r, Stroke::new(1.2_f32, line()));
    let at = |a: f32, rr: f32| c + Vec2::new(a.cos(), a.sin()) * rr;
    for k in 0..5 {
        let a = angle + k as f32 * TAU / 5.0;
        p.add(egui::Shape::convex_polygon(
            vec![
                at(a, r * 0.28),
                at(a + 0.6, r * 0.62),
                at(a + 1.0, r * 0.86),
                at(a + 0.45, r * 0.86),
                at(a + 0.05, r * 0.52),
            ],
            tint(blade, if spinning { 220 } else { 160 }),
            Stroke::NONE,
        ));
    }
    p.circle_filled(c, r * 0.26, Color32::from_rgb(30, 32, 38));
    p.circle_stroke(c, r * 0.26, Stroke::new(1.0_f32, tint(blade, 160)));
}

/// Mode glyphs for the three preset cards.
fn paint_mode_icon(p: &egui::Painter, c: Pos2, which: usize, col: Color32) {
    let s = Stroke::new(1.8_f32, col);
    match which {
        // Apple Default: a circle with a check-like "auto" tick
        0 => {
            p.circle_stroke(c, 9.0, s);
            p.line_segment([c + Vec2::new(-4.0, 0.5), c + Vec2::new(-1.0, 3.5)], s);
            p.line_segment([c + Vec2::new(-1.0, 3.5), c + Vec2::new(4.5, -3.0)], s);
        }
        // Smart: a gauge arc with a needle
        1 => {
            arc(p, c + Vec2::new(0.0, 2.0), 9.0, PI * 0.95, PI * 2.05, 1.8, |_| col);
            p.line_segment([c + Vec2::new(0.0, 2.0), c + Vec2::new(4.5, -3.5)], s);
            p.circle_filled(c + Vec2::new(0.0, 2.0), 2.0, col);
        }
        // Full Blast: wind lines
        _ => {
            for (dy, len) in [(-5.0, 14.0), (0.0, 18.0), (5.0, 11.0)] {
                p.line_segment([c + Vec2::new(-9.0, dy), c + Vec2::new(-9.0 + len, dy)], s);
            }
        }
    }
}

/// Friendlier names for a few sensors (display only; the helper is unaffected).
fn display_name(r: &Reading) -> &str {
    match r.key.as_str() {
        "TaLP" => "Case Air Left",
        "TaRF" => "Case Air Right",
        "TW0P" => "Wi-Fi Module",
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
const GROUPS: [&str; 5] = ["PROCESSOR", "GRAPHICS", "BATTERY", "SYSTEM", "UNIDENTIFIED"];

/// Minimal PNG writer (stored deflate) for the screenshot/design-review mode.
fn write_png(path: &str, img: &egui::ColorImage) -> std::io::Result<()> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    let [w, h] = img.size;
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let c = img.pixels[y * w + x];
            raw.extend_from_slice(&[c.r(), c.g(), c.b()]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, ch) in chunks.iter().enumerate() {
        z.push((i + 1 == chunks.len()) as u8);
        let n = ch.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(ch);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |ty: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = ty.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::write(path, out)
}

// ---------------------------------------------------------------- app

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        let mut v = egui::Visuals::dark();
        v.panel_fill = Color32::TRANSPARENT;
        v.window_fill = Color32::from_rgb(28, 30, 36);
        v.window_rounding = Rounding::same(CARD_RADIUS);
        v.window_stroke = Stroke::new(1.0_f32, line());
        v.window_shadow = Shadow {
            offset: Vec2::new(0.0, 12.0),
            blur: 40.0,
            spread: 0.0,
            color: Color32::from_black_alpha(140),
        };
        v.popup_shadow = v.window_shadow;
        v.selection.bg_fill = ACCENT;
        v.selection.stroke = Stroke::new(1.0_f32, CYAN);
        v.override_text_color = Some(TEXT);
        v.slider_trailing_fill = true;
        v.extreme_bg_color = Color32::from_rgb(17, 18, 22);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.rounding = Rounding::same(8.0);
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, line());
        v.widgets.inactive.weak_bg_fill = RAISED;
        v.widgets.inactive.bg_fill = TRACK;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, line());
        v.widgets.hovered.weak_bg_fill = mix(RAISED, Color32::WHITE, 0.08);
        v.widgets.hovered.bg_fill = mix(TRACK, Color32::WHITE, 0.1);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, tint(CYAN, 90));
        v.widgets.active.bg_fill = ACCENT;
        v.widgets.active.weak_bg_fill = mix(RAISED, Color32::WHITE, 0.12);
        cc.egui_ctx.set_visuals(v);
        cc.egui_ctx.style_mut(|st| {
            st.spacing.item_spacing = Vec2::new(8.0, 8.0);
            st.spacing.interact_size.y = 24.0;
            st.spacing.button_padding = Vec2::new(10.0, 4.0);
            st.text_styles.insert(egui::TextStyle::Body, font(TEXT_SIZE));
            st.text_styles.insert(egui::TextStyle::Button, font(TEXT_SIZE));
        });

        let s = settings::load();
        let pending = if is_root() {
            Preset::from_key(&s.preset)
        } else {
            None
        };
        let helper = helper::status();
        let ctx = cc.egui_ctx.clone();
        let shot = std::env::var("MACSMARTCOOLER_SHOT")
            .ok()
            .map(|p| (p, std::time::Instant::now(), false));
        let tray = if shot.is_none() {
            let c = cc.egui_ctx.clone();
            crate::tray::Tray::new(move || c.request_repaint())
        } else {
            None
        };
        App {
            helper,
            helper_checked: Some(std::time::Instant::now()),
            worker: Worker::spawn(move || ctx.request_repaint()),
            dialog: None,
            show_prefs: std::env::var_os("MACSMARTCOOLER_SHOT_PREFS").is_some(),
            saved: s.clone(),
            s,
            pending,
            helper_synced: false,
            chosen: None,
            // never prompt for a password while just taking a screenshot
            auto_updated: shot.is_some(),
            model: crate::load::machine_title(),
            rpm: Vec::new(),
            hist: Vec::new(),
            hist_at: None,
            tray,
            quitting: false,
            shot,
        }
    }

    fn temp(&self, c: f64) -> String {
        if self.s.fahrenheit {
            format!("{:.0}°F", c * 9.0 / 5.0 + 32.0)
        } else {
            format!("{c:.0}°C")
        }
    }

    /// Number only, with a degree sign (for big numerals).
    fn deg(&self, c: f64) -> String {
        if self.s.fahrenheit {
            format!("{:.0}°", c * 9.0 / 5.0 + 32.0)
        } else {
            format!("{c:.0}°")
        }
    }

    fn smart_mode(&self) -> FanMode {
        FanMode::Smart {
            lo: self.s.smart_lo,
            hi: self.s.smart_hi,
        }
    }

    fn preset(&self, snap: &Snapshot) -> Preset {
        let all_auto = snap.modes.iter().all(|m| *m == FanMode::Auto);
        if all_auto && snap.fans.iter().any(|f| f.manual) {
            Preset::Custom // something else is driving the fans
        } else if snap.modes.is_empty() || all_auto {
            Preset::Automatic
        } else if snap.modes.iter().all(|m| matches!(m, FanMode::Smart { .. })) {
            Preset::Smart
        } else if snap
            .modes
            .iter()
            .zip(&snap.fans)
            .all(|(m, f)| matches!(m, FanMode::Constant(r) if *r >= f.max))
        {
            Preset::FullBlast
        } else {
            Preset::Custom
        }
    }

    fn apply_preset(&self, p: Preset, snap: &Snapshot) {
        for f in &snap.fans {
            let mode = match p {
                Preset::Automatic => FanMode::Auto,
                Preset::Smart => self.smart_mode(),
                Preset::FullBlast => FanMode::Constant(f.max),
                Preset::Custom => continue,
            };
            self.worker.send(Cmd::SetMode(f.index, mode));
        }
    }

    fn choose_preset(&mut self, ctx: &egui::Context, p: Preset, snap: &Snapshot) {
        self.s.preset = p.key().into();
        settings::save(&self.s);
        self.saved = self.s.clone();
        if snap.can_control || p == Preset::Automatic {
            self.chosen = Some((p, std::time::Instant::now()));
            self.apply_preset(p, snap);
        } else {
            // applied as soon as the helper is up; only ask for the password if the
            // helper is actually missing or outdated (not just still starting)
            self.pending = Some(p);
            if snap.root || matches!(helper::status(), helper::Status::NotInstalled | helper::Status::Outdated) {
                request_install(ctx.clone());
            }
        }
    }

    fn screenshot_step(&mut self, ctx: &egui::Context, ready: bool) {
        let Some((path, started, requested)) = self.shot.clone() else {
            return;
        };
        for e in ctx.input(|i| i.raw.events.clone()) {
            if let egui::Event::Screenshot { image, .. } = e {
                if let Err(err) = write_png(&path, &image) {
                    eprintln!("screenshot: {err}");
                }
                std::process::exit(0);
            }
        }
        let wait = std::env::var("MACSMARTCOOLER_SHOT_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(2.5);
        if !requested && ready && started.elapsed() > Duration::from_secs_f64(wait) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            self.shot = Some((path, started, true));
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[allow(unused_mut)]
        let mut snap = self
            .worker
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.screenshot_step(ctx, snap.ready);
        // MACSMARTCOOLER_SHOT_STATE (design review only, display-only, comma separated):
        // error, outdated, nohelper, f, unnamed, size=WxH, preset=auto|full|custom
        let shot_state = if self.shot.is_some() { std::env::var("MACSMARTCOOLER_SHOT_STATE").unwrap_or_default() } else { String::new() };
        let has = |k: &str| shot_state.split(',').any(|x| x == k);
        if has("error") {
            snap.error = Some("Could not set Left side fan: SMC write failed (retrying in 30 s)".into());
        }
        if has("nohelper") {
            snap.can_control = false;
        }
        if has("f") {
            self.s.fahrenheit = true;
        }
        if has("unnamed") {
            self.s.show_unnamed = true;
        }
        if let Some(sz) = shot_state.split(',').find_map(|x| x.strip_prefix("size=")) {
            if let Some((w, h)) = sz.split_once('x') {
                if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
                    if (ctx.screen_rect().width() - w).abs() > 1.0 {
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(w, h)));
                    }
                }
            }
        }
        // MACSMARTCOOLER_SHOT_DIALOG: open fan 0's dialog on the sensor page, then flip to fixed speed
        let dialog_env = std::env::var("MACSMARTCOOLER_SHOT_DIALOG").ok();
        if self.shot.is_some() && dialog_env.is_some() && snap.ready {
            let keep_sensor = dialog_env.as_deref() == Some("sensor");
            let el = self.shot.as_ref().map_or(0.0, |s| s.1.elapsed().as_secs_f32());
            if self.dialog.is_none() && el < 1.5 {
                if let Some(f) = snap.fans.first() {
                    self.open_dialog(f, &FanMode::Auto);
                    if let Some(d) = &mut self.dialog {
                        d.kind = DialogKind::Sensor;
                    }
                }
            } else if el > 1.5 && !keep_sensor {
                if let Some(d) = &mut self.dialog {
                    d.kind = DialogKind::Constant;
                }
            }
        }

        let installing = INSTALLING.load(Ordering::SeqCst);
        if installing {
            self.helper_checked = None;
        } else if self
            .helper_checked
            .map_or(true, |t| t.elapsed() > Duration::from_secs(3))
        {
            self.helper = helper::status();
            self.helper_checked = Some(std::time::Instant::now());
            // The helper is what actually drives the fans; an old one keeps running old
            // logic after the app is updated. Ask once per launch to replace it.
            if self.helper == helper::Status::Outdated && !snap.root && !self.auto_updated {
                self.auto_updated = true;
                request_install(ctx.clone());
            }
        }
        if self.helper != helper::Status::Installed && !snap.root {
            ctx.request_repaint_after(Duration::from_secs(3)); // keep re-checking while not ready
        }
        if let Some(p) = self.pending {
            if snap.ready && snap.can_control {
                self.apply_preset(p, &snap);
                self.pending = None;
            }
        }
        // choices made before the helper was installed only reached the user's private copy
        if snap.can_control && !snap.root && !self.helper_synced && self.shot.is_none() {
            settings::save(&self.s);
            self.helper_synced = true;
        }

        // Fan speed as shown: the tachometer wobbles +-20-50 rpm second to second even at a
        // fixed target, so show a ~3 s average (rounded to 10 rpm when drawn). No continuous
        // animation: the window only redraws when new readings arrive.
        if snap.ready && self.hist_at.map_or(true, |t| t.elapsed() >= Duration::from_millis(900)) {
            self.hist_at = Some(std::time::Instant::now());
            self.rpm.resize(snap.fans.len(), None);
            self.hist.resize(snap.fans.len(), Default::default());
            for ((h, e), f) in self.hist.iter_mut().zip(self.rpm.iter_mut()).zip(&snap.fans) {
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

        let mut preset = self.preset(&snap);
        if let Some((p, t)) = self.chosen {
            if p == preset || t.elapsed() > Duration::from_secs(4) {
                self.chosen = None;
            } else {
                preset = p;
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        self.menu_bar(ctx, &snap, &mut preset);
        if let Some(p) = shot_state.split(',').find_map(|x| x.strip_prefix("preset=")) {
            preset = match p {
                "auto" => Preset::Automatic,
                "full" => Preset::FullBlast,
                _ => Preset::Custom,
            };
        }
        if has("outdated") {
            self.helper = helper::Status::Outdated;
        }

        // ---------- window background: soft vertical gradient ----------
        {
            let r = ctx.screen_rect();
            let p = ctx.layer_painter(egui::LayerId::background());
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(r.left_top(), BG_TOP);
            mesh.colored_vertex(r.right_top(), BG_TOP);
            mesh.colored_vertex(r.right_bottom(), BG_BOTTOM);
            mesh.colored_vertex(r.left_bottom(), BG_BOTTOM);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            p.add(egui::Shape::mesh(mesh));
        }

        // ---------- title bar (transparent; traffic lights sit on the left) ----------
        egui::TopBottomPanel::top("titlebar")
            .show_separator_line(false)
            .exact_height(TITLEBAR + 8.0)
            .frame(egui::Frame::none().inner_margin(Margin {
                left: 86.0,
                right: 14.0,
                top: 1.0,
                bottom: 0.0,
            }))
            .show(ctx, |ui| {
                // drag the window from anywhere in the bar
                let bar = ui.max_rect();
                let drag = ui.interact(bar, Id::new("titlebar_drag"), Sense::click_and_drag());
                if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    let max = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
                }
                ui.horizontal(|ui| {
                    ui.set_height(28.0);
                    ui.label(RichText::new("Mac Smart Cooler").font(font(14.0)).color(TEXT));
                    ui.label(RichText::new(&self.model).font(font(12.0)).color(FAINT))
                        .on_hover_text(format!("Model identifier: {}", crate::smc::hw_model()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if gear_button(ui).clicked() {
                            self.show_prefs = !self.show_prefs;
                        }
                        ui.add_space(2.0);
                        if installing {
                            pill(ui, "Waiting for password…", AMBER);
                        } else if self.helper == helper::Status::Outdated && !snap.root {
                            if button(ui, "Update helper", true)
                                .on_hover_text("The background helper is older than this app. One password prompt updates it.")
                                .clicked()
                            {
                                request_install(ctx.clone());
                            }
                        } else if self.helper == helper::Status::Unreachable && !snap.root {
                            if button(ui, "Repair helper", true)
                                .on_hover_text("The background helper isn't answering. One password prompt reinstalls it.")
                                .clicked()
                            {
                                request_install(ctx.clone());
                            }
                        } else if snap.can_control {
                            pill(ui, "Fan control on", GREEN);
                        } else {
                            if button(ui, "Enable fan control", true)
                                .on_hover_text("Installs a small background helper. Asks for your password once, never again.")
                                .clicked()
                            {
                                request_install(ctx.clone());
                            }
                            pill(ui, if snap.ready { "Monitoring only" } else { "Starting…" }, AMBER);
                        }
                    });
                });
            });

        // ---------- sensors ----------
        egui::SidePanel::right("sensors")
            .show_separator_line(false)
            .resizable(false)
            .exact_width(320.0)
            .frame(egui::Frame::none().inner_margin(Margin {
                left: 0.0,
                right: 20.0,
                top: 6.0,
                bottom: 20.0,
            }))
            .show(ctx, |ui| {
                card_frame().show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    self.sensors_list(ui, &snap);
                });
            });

        // ---------- main column ----------
        egui::CentralPanel::default()
            .frame(egui::Frame::none().inner_margin(Margin {
                left: 20.0,
                right: GAP,
                top: 6.0,
                bottom: 20.0,
            }))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = GAP;
                self.hero(ui, &snap, preset);
                if let Some(p) = self.mode_cards(ui, preset) {
                    self.choose_preset(ctx, p, &snap);
                }
                self.fan_cards(ui, &snap);
                if let Some(e) = &snap.error {
                    egui::Frame::none()
                        .fill(tint(RED, 26))
                        .stroke(Stroke::new(1.0_f32, tint(RED, 60)))
                        .rounding(Rounding::same(RADIUS))
                        .inner_margin(Margin::symmetric(14.0_f32, 9.0_f32))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(e).font(font(12.5)).color(mix(RED, TEXT, 0.35)));
                        });
                }
            });

        self.custom_dialog(ctx, &snap);
        self.prefs_window(ctx, &snap, preset);

        if self.s != self.saved && self.shot.is_none() {
            settings::save(&self.s);
            self.saved = self.s.clone();
        }
    }
}

impl App {
    /// Menu bar item: handle clicks, keep its status and checkmarks current, and keep the
    /// app running in the menu bar when the window is closed.
    fn menu_bar(&mut self, ctx: &egui::Context, snap: &Snapshot, preset: &mut Preset) {
        if self.tray.is_none() {
            return;
        }
        let actions = self.tray.as_ref().map(|t| t.actions()).unwrap_or_default();
        for a in actions {
            match a {
                crate::tray::Action::Mode(i) => {
                    let p = [Preset::Automatic, Preset::Smart, Preset::FullBlast][i];
                    if p != *preset {
                        self.choose_preset(ctx, p, snap);
                        *preset = p;
                    }
                }
                crate::tray::Action::Open => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                crate::tray::Action::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        // closing the window hides it; Mac Smart Cooler keeps running in the menu bar
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        let hot = snap.sensors.iter().find(|r| r.key == HOT_KEY).map(|r| r.value);
        let title = hot.map(|t| self.deg(t)).unwrap_or_default();
        // one short line: "62°C · Fans off · Quiet"
        let fans = if snap.fans.iter().all(|f| f.actual < 1.0) {
            "Fans off".to_string()
        } else {
            format!("{} rpm", snap.fans.iter().map(|f| thousands(f.actual)).collect::<Vec<_>>().join(" / "))
        };
        let state = match *preset {
            Preset::Automatic => "macOS".to_string(),
            Preset::Smart => snap
                .activity
                .smart
                .as_deref()
                .and_then(|w| w.split(':').next())
                .unwrap_or("Starting")
                .to_string(),
            Preset::FullBlast => "Full Blast".into(),
            Preset::Custom => "Custom".into(),
        };
        let status = match hot {
            Some(t) => format!("{} · {fans} · {state}", self.temp(t)),
            None => "Reading sensors…".into(),
        };
        let sel = match *preset {
            Preset::Automatic => Some(0),
            Preset::Smart => Some(1),
            Preset::FullBlast => Some(2),
            Preset::Custom => None,
        };
        if let Some(t) = &mut self.tray {
            t.update(&title, &status, sel);
        }
    }

    /// Big status card: hottest-core ring gauge, what Smart Cooling is doing, live load.
    fn hero(&self, ui: &mut egui::Ui, snap: &Snapshot, preset: Preset) {
        let a = &snap.activity;
        let hot = snap.sensors.iter().find(|r| r.key == HOT_KEY).map(|r| r.value);
        card_frame().inner_margin(Margin::same(22.0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // ring gauge
                let size = 148.0;
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
                let p = ui.painter();
                let c = rect.center();
                let r = size / 2.0 - 8.0;
                let (a0, a1) = (PI * 0.75, PI * 2.25);
                arc(p, c, r, a0, a1, 9.0, |_| TRACK);
                if let Some(t) = hot {
                    let shown = ui.ctx().animate_value_with_time(Id::new("gauge"), t as f32, 0.25);
                    let frac = ((shown - 30.0) / 70.0).clamp(0.02, 1.0);
                    arc(p, c, r, a0, a0 + (a1 - a0) * frac, 9.0, |u| {
                        temp_color(30.0 + (u * frac * 70.0) as f64)
                    });
                    p.text(c + Vec2::new(0.0, -6.0), Align2::CENTER_CENTER, self.deg(shown as f64), num(40.0), TEXT);
                    let arrow = if a.trend > 0.5 { "↑" } else if a.trend < -0.5 { "↓" } else { "→" };
                    p.text(
                        c + Vec2::new(0.0, 24.0),
                        Align2::CENTER_CENTER,
                        format!("{arrow} {:+.1}°/min", if self.s.fahrenheit { a.trend * 1.8 } else { a.trend }),
                        num(12.0),
                        DIM,
                    );
                    p.text(c + Vec2::new(0.0, r + 2.0), Align2::CENTER_CENTER, "HOTTEST CORE", font(10.0), FAINT);
                } else {
                    p.text(c, Align2::CENTER_CENTER, "—", num(36.0), FAINT);
                }

                ui.add_space(20.0);
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    let (label, title, detail) = match preset {
                        Preset::Automatic => (
                            "APPLE DEFAULT",
                            "macOS is in control".to_string(),
                            "Fans follow Apple's own curve and often stay off until the chip is already hot.".to_string(),
                        ),
                        Preset::Smart => {
                            let why = a.smart.clone().unwrap_or_else(|| "Quiet: starting up".into());
                            let (t, d) = why.split_once(": ").unwrap_or((why.as_str(), ""));
                            let mut d = d.to_string();
                            if let Some(f) = d.get(0..1) {
                                d = f.to_uppercase() + &d[1..];
                            }
                            ("SMART COOLING", t.to_string(), d)
                        }
                        Preset::FullBlast => ("FULL BLAST", "Maximum airflow".into(), "Every fan is running at full speed.".into()),
                        Preset::Custom => ("CUSTOM", "Per-fan settings".into(), "Each fan follows its own setting below.".into()),
                    };
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(RichText::new(label).font(font(11.0)).color(CYAN));
                    ui.label(RichText::new(title).font(font(26.0)).color(TEXT));
                    ui.label(RichText::new(detail).font(font(13.0)).color(DIM));
                    ui.add_space(16.0);

                    // live meters
                    let mut items: Vec<(&str, String, f32, Color32)> = vec![(
                        "CPU",
                        format!("{:.0}%", a.cpu * 100.0),
                        a.cpu as f32,
                        load_color(a.cpu),
                    )];
                    if let Some(g) = a.gpu {
                        items.push(("GPU", format!("{:.0}%", g * 100.0), g as f32, load_color(g)));
                    }
                    if let Some(c) = a.chassis {
                        items.push(("PALM REST", self.temp(c), ((c - 20.0) / 25.0) as f32, temp_color(c + 12.0)));
                    }
                    if let Some(b) = a.body {
                        items.push(("CASE AIR", self.temp(b), ((b - 28.0) / 36.0) as f32, temp_color(b + 20.0)));
                    }
                    let w = ((ui.available_width() - 18.0 * (items.len() as f32 - 1.0)) / items.len() as f32).max(60.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 18.0;
                        for (i, (label, value, frac, col)) in items.iter().enumerate() {
                            let (r, _) = ui.allocate_exact_size(Vec2::new(w, 46.0), Sense::hover());
                            let p = ui.painter();
                            p.text(r.left_top(), Align2::LEFT_TOP, *label, font(10.0), FAINT);
                            p.text(r.left_top() + Vec2::new(0.0, 14.0), Align2::LEFT_TOP, value, num(18.0), TEXT);
                            let f = ui.ctx().animate_value_with_time(Id::new(("meter", i)), *frac, 0.25);
                            meter(
                                p,
                                Rect::from_min_size(r.left_bottom() - Vec2::new(0.0, 4.0), Vec2::new(w, 4.0)),
                                f,
                                mix(*col, CARD, 0.35),
                                *col,
                            );
                        }
                    });
                });
            });
        });
    }

    /// Three large preset cards. Returns the one clicked.
    fn mode_cards(&self, ui: &mut egui::Ui, preset: Preset) -> Option<Preset> {
        let opts = [
            (Preset::Automatic, "Apple Default", "macOS decides"),
            (Preset::Smart, "Smart Cooling", "Quiet until it matters"),
            (Preset::FullBlast, "Full Blast", "Every fan at maximum"),
        ];
        let gap = 12.0;
        let w = (ui.available_width() - gap * 2.0) / 3.0;
        let mut out = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (i, (p, title, sub)) in opts.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 64.0), Sense::click());
                let sel = preset == *p;
                let hov = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
                let on = ui.ctx().animate_bool_with_time(resp.id.with("sel"), sel, 0.18);
                let painter = ui.painter();
                let fill = mix(mix(CARD, RAISED, hov * 0.6), mix(CARD, ACCENT, 0.16), on);
                let stroke = Stroke::new(1.0 + on * 0.5, mix(line_solid(), ACCENT, on));
                painter.rect(rect, Rounding::same(14.0), fill, stroke);
                let icol = mix(DIM, CYAN, on);
                let ic = rect.left_center() + Vec2::new(28.0, 0.0);
                painter.circle_filled(ic, 17.0, mix(RAISED, ACCENT, on * 0.25));
                paint_mode_icon(painter, ic, i, icol);
                painter.text(rect.left_center() + Vec2::new(56.0, -8.0), Align2::LEFT_CENTER, *title, font(14.0), TEXT);
                painter.text(rect.left_center() + Vec2::new(56.0, 10.0), Align2::LEFT_CENTER, *sub, font(11.5), mix(FAINT, DIM, on));
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !sel {
                    out = Some(*p);
                }
            }
        });
        out
    }

    fn fan_cards(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        if snap.ready && snap.fans.is_empty() {
            card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("No fans found (fanless Mac?)").color(DIM));
            });
            return;
        }
        let n = snap.fans.len().max(1);
        let gap = 12.0;
        let w = (ui.available_width() - gap * (n as f32 - 1.0)) / n as f32;
        let h = (ui.available_height() - if snap.error.is_some() { 52.0 } else { 0.0 }).max(150.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for fan in &snap.fans {
                self.fan_card(ui, fan, snap, Vec2::new(w, h));
            }
        });
    }

    fn fan_card(&mut self, ui: &mut egui::Ui, fan: &FanInfo, snap: &Snapshot, size: Vec2) {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let p = ui.painter().clone();
        p.rect(rect, Rounding::same(CARD_RADIUS), CARD, Stroke::new(1.0_f32, line()));
        let inner = rect.shrink(18.0);

        let mode = snap.modes.get(fan.index).cloned().unwrap_or(FanMode::Auto);
        let status = match &mode {
            FanMode::Auto if fan.manual => "Set by another app".to_string(),
            FanMode::Auto => "macOS".into(),
            FanMode::Constant(r) => format!("Fixed {} rpm", thousands(*r)),
            FanMode::Smart { .. } if fan.manual => "Smart Cooling · active".into(),
            FanMode::Smart { .. } => "Smart Cooling · resting".into(),
            FanMode::Sensor { key, lo, hi } => {
                let name = snap
                    .sensors
                    .iter()
                    .find(|r| &r.key == key)
                    .map(|r| r.name.as_str())
                    .unwrap_or(key);
                format!("{name} {}–{}", self.temp(*lo), self.temp(*hi))
            }
        };

        // header: glyph, name, status
        let spinning = fan.actual > 0.0;
        paint_fan(&p, inner.left_top() + Vec2::new(16.0, 16.0), 16.0, 0.3, spinning);
        // Auto / Smart / Custom on the right; name and status get the rest, cut with "…" if needed
        let seg_w = 186.0;
        let text_w = (inner.width() - seg_w - 42.0 - 12.0).max(40.0);
        let name = elided(&p, &fan.name, font(14.0), TEXT, text_w);
        p.galley(inner.left_top() + Vec2::new(42.0, 7.0 - name.size().y / 2.0), name, TEXT);
        let st = elided(&p, &status, font(11.5), FAINT, text_w);
        p.galley(inner.left_top() + Vec2::new(42.0, 25.0 - st.size().y / 2.0), st, FAINT);

        let mut seg_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_size(Pos2::new(inner.right() - seg_w, inner.top() + 3.0), Vec2::new(seg_w, 26.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let is_auto = mode == FanMode::Auto && !fan.manual;
        let is_smart = matches!(mode, FanMode::Smart { .. });
        let sel = if is_auto { 0 } else if is_smart { 1 } else { 2 };
        match segmented(&mut seg_ui, seg_w, Id::new(("fan_ctl", fan.index)), &["Auto", "Smart", "Custom"], Some(sel)) {
            Some(0) if !is_auto => {
                self.worker.send(Cmd::SetMode(fan.index, FanMode::Auto));
            }
            Some(1) if !is_smart => {
                self.worker.send(Cmd::SetMode(fan.index, self.smart_mode()));
            }
            Some(2) => self.open_dialog(fan, &mode),
            _ => {}
        }

        // big number + bar
        let smooth = self.rpm.get(fan.index).copied().flatten().unwrap_or(fan.actual as f32);
        let rpm = (smooth / 10.0).round() * 10.0;
        let base = inner.left_bottom() - Vec2::new(0.0, 34.0);

        // rpm history (last 2 minutes) in the space between header and number
        let chart = Rect::from_min_max(
            Pos2::new(inner.left(), inner.top() + 50.0),
            Pos2::new(inner.right(), base.y - 52.0),
        );
        if chart.height() > 24.0 && fan.max > 0.0 {
            if let Some(h) = self.hist.get(fan.index).filter(|h| h.len() >= 2) {
                sparkline(&p, chart, h, fan.max as f32);
            }
            p.text(chart.left_top(), Align2::LEFT_TOP, "LAST 2 MIN", font(9.5), FAINT);
        }
        let g = p.text(base, Align2::LEFT_BOTTOM, thousands(rpm as f64), num(38.0), TEXT);
        p.text(Pos2::new(g.right() + 6.0, g.bottom() - 8.0), Align2::LEFT_BOTTOM, "rpm", font(13.0), FAINT);
        if fan.max > 0.0 {
            let pct = (rpm as f64 / fan.max * 100.0).round();
            p.text(Pos2::new(inner.right(), g.bottom() - 8.0), Align2::RIGHT_BOTTOM, format!("{pct:.0}%"), num(13.0), DIM);
            let bar = Rect::from_min_size(inner.left_bottom() - Vec2::new(0.0, 22.0), Vec2::new(inner.width(), 6.0));
            meter(&p, bar, (rpm as f64 / fan.max) as f32, ACCENT, CYAN);
            let mx = bar.left() + bar.width() * (fan.min / fan.max).clamp(0.0, 1.0) as f32;
            p.line_segment([Pos2::new(mx, bar.top() - 3.0), Pos2::new(mx, bar.bottom() + 3.0)], Stroke::new(1.5_f32, tint(TEXT, 120)));
            p.text(inner.left_bottom(), Align2::LEFT_BOTTOM, format!("min {}", thousands(fan.min)), num(11.0), FAINT);
            p.text(inner.right_bottom(), Align2::RIGHT_BOTTOM, format!("max {}", thousands(fan.max)), num(11.0), FAINT);
        }
    }

    fn sensors_list(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Sensors").font(font(16.0)).color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let n = snap.sensors.iter().filter(|r| r.named || self.s.show_unnamed).count();
                ui.label(RichText::new(format!("{n}")).font(num(12.0)).color(FAINT));
            });
        });
        if !snap.ready {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Reading sensors…").color(DIM));
            });
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                for (g, title) in GROUPS.iter().enumerate() {
                    if g == 4 && !self.s.show_unnamed {
                        continue;
                    }
                    let rows: Vec<&Reading> = snap.sensors.iter().filter(|r| group_of(r) == g).collect();
                    if rows.is_empty() {
                        continue;
                    }
                    ui.add_space(10.0);
                    ui.label(RichText::new(*title).font(font(10.5)).color(FAINT));
                    ui.add_space(2.0);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (i, r) in rows.iter().enumerate() {
                        self.sensor_row(ui, r, i + 1 < rows.len());
                    }
                    ui.spacing_mut().item_spacing.y = 8.0;
                }
            });
    }

    fn sensor_row(&self, ui: &mut egui::Ui, r: &Reading, divider: bool) {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width() - 6.0, 30.0), Sense::hover());
        let p = ui.painter();
        if resp.hovered() {
            p.rect_filled(rect.expand2(Vec2::new(4.0, 0.0)), Rounding::same(6.0), tint(Color32::WHITE, 6));
        }
        let color = temp_color(r.value);
        p.circle_filled(rect.left_center() + Vec2::new(4.0, 0.0), 3.0, color);
        p.text(rect.left_center() + Vec2::new(16.0, 0.0), Align2::LEFT_CENTER, display_name(r), font(13.0), mix(DIM, TEXT, 0.6));
        p.text(rect.right_center(), Align2::RIGHT_CENTER, self.temp(r.value), num(13.0), color);
        if divider {
            p.line_segment(
                [rect.left_bottom() + Vec2::new(16.0, 0.0), rect.right_bottom()],
                Stroke::new(1.0_f32, tint(Color32::WHITE, 9)),
            );
        }
        resp.on_hover_text(format!("SMC key {}", r.key));
    }

    fn open_dialog(&mut self, fan: &FanInfo, mode: &FanMode) {
        let mut d = CustomDialog {
            fan: fan.index,
            kind: DialogKind::Constant,
            rpm: ((fan.min + fan.max) / 2.0 / 100.0).round() * 100.0,
            sensor: HOT_KEY.to_string(),
            lo: self.s.smart_lo,
            hi: self.s.smart_hi,
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
    }

    fn custom_dialog(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let Some(mut d) = self.dialog.take() else {
            return;
        };
        let Some(fan) = snap.fans.get(d.fan) else {
            return;
        };
        if !(fan.max > fan.min) {
            return;
        }
        let mut keep = true;
        // a fresh window id per page, so it sizes to that page instead of keeping the
        // size of the taller one
        egui::Window::new("custom_fan")
            .id(Id::new(("custom_fan", d.kind == DialogKind::Constant)))
            .title_bar(false)
            .collapsible(false)
            .auto_sized()
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(egui::Frame::window(&ctx.style()).inner_margin(Margin::same(22.0)))
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.label(RichText::new(format!("{} fan", fan.name)).font(font(18.0)));
                ui.label(RichText::new("Choose how this fan should run.").font(font(12.5)).color(DIM));
                ui.add_space(10.0);
                let sel = segmented(ui, 420.0, Id::new("dlg_kind"), &["Fixed speed", "Follow a sensor"], Some(if d.kind == DialogKind::Constant { 0 } else { 1 }));
                if let Some(i) = sel {
                    d.kind = if i == 0 { DialogKind::Constant } else { DialogKind::Sensor };
                }
                ui.add_space(14.0);
                if d.kind == DialogKind::Constant {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(thousands(d.rpm)).font(num(30.0)));
                        ui.label(RichText::new("rpm").color(FAINT));
                    });
                    ui.spacing_mut().slider_width = 420.0;
                    ui.add(egui::Slider::new(&mut d.rpm, fan.min..=fan.max).step_by(50.0).show_value(false));
                    d.rpm = ((d.rpm / 50.0).round() * 50.0).clamp(fan.min, fan.max);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("min {}", thousands(fan.min))).font(num(11.0)).color(FAINT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(RichText::new(format!("max {}", thousands(fan.max))).font(num(11.0)).color(FAINT));
                        });
                    });
                } else {
                    let sel = snap
                        .sensors
                        .iter()
                        .find(|r| r.key == d.sensor)
                        .map(|r| display_name(r).to_string())
                        .unwrap_or_else(|| d.sensor.clone());
                    egui::ComboBox::new("sensor_pick", "")
                        .selected_text(sel)
                        .width(420.0)
                        .show_ui(ui, |ui| {
                            for r in snap.sensors.iter().filter(|r| r.named || self.s.show_unnamed) {
                                ui.selectable_value(&mut d.sensor, r.key.clone(), format!("{}  ({})", display_name(r), self.temp(r.value)));
                            }
                        });
                    ui.add_space(6.0);
                    egui::Grid::new("curve").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                        ui.label(RichText::new("Start speeding up at").color(DIM));
                        ui.add(unit_drag(egui::DragValue::new(&mut d.lo).range(20.0..=110.0), self.s.fahrenheit));
                        ui.end_row();
                        ui.label(RichText::new("Full speed at").color(DIM));
                        ui.add(unit_drag(egui::DragValue::new(&mut d.hi).range(20.0..=110.0), self.s.fahrenheit));
                        ui.end_row();
                    });
                    ui.label(
                        RichText::new(format!("Below {} the fan runs at its minimum ({} rpm).", self.temp(d.lo), thousands(fan.min)))
                            .font(font(12.0))
                            .color(FAINT),
                    );
                }
                if !snap.can_control {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Enable fan control (top right) first.").color(AMBER));
                }
                ui.add_space(16.0);
                // one row tall (a bare right-to-left layout would take all the height)
                ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 28.0), Layout::right_to_left(Align::Center), |ui| {
                    if button(ui, "Apply", true).clicked() {
                        let mode = match d.kind {
                            DialogKind::Constant => FanMode::Constant(d.rpm),
                            DialogKind::Sensor => FanMode::Sensor {
                                key: d.sensor.clone(),
                                lo: d.lo,
                                hi: d.hi.max(d.lo + 1.0),
                            },
                        };
                        self.worker.send(Cmd::SetMode(d.fan, mode));
                        keep = false;
                    }
                    if button(ui, "Cancel", false).clicked() {
                        keep = false;
                    }
                });
            });
        if keep {
            self.dialog = Some(d);
        }
    }

    fn prefs_window(&mut self, ctx: &egui::Context, snap: &Snapshot, preset: Preset) {
        if !self.show_prefs {
            return;
        }
        let (old_lo, old_hi) = (self.s.smart_lo, self.s.smart_hi);
        let mut close = false;
        egui::Window::new("prefs")
            .title_bar(false)
            .collapsible(false)
            .auto_sized()
            .anchor(Align2::RIGHT_TOP, [-20.0, TITLEBAR + 14.0])
            .frame(egui::Frame::window(&ctx.style()).inner_margin(Margin::same(22.0)))
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Preferences").font(font(18.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if button(ui, "Done", false).clicked() {
                            close = true;
                        }
                    });
                });
                ui.add_space(8.0);

                ui.label(RichText::new("DISPLAY").font(font(10.5)).color(FAINT));
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Temperature unit").color(DIM));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(i) = segmented(ui, 110.0, Id::new("unit"), &["°C", "°F"], Some(self.s.fahrenheit as usize)) {
                            self.s.fahrenheit = i == 1;
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Show unidentified SMC sensors").color(DIM));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        toggle(ui, &mut self.s.show_unnamed);
                    });
                });

                ui.add_space(10.0);
                ui.label(RichText::new("SMART COOLING").font(font(10.5)).color(FAINT));
                ui.label(
                    RichText::new(
                        "Fans stay with macOS until the heat is real: warm air in the back of the case or a \
                         warm palm rest while the Mac is busy, or minutes of heavy work with a hot chip. \
                         Launches and brief spikes never spin them up. The temperatures below are for the chip.",
                    )
                    .font(font(12.0))
                    .color(DIM),
                );
                ui.add_space(4.0);
                ui.spacing_mut().slider_width = 220.0;
                egui::Grid::new("smart").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
                    ui.label(RichText::new("Cooling starts at").color(DIM));
                    let f = self.s.fahrenheit;
                    ui.add(unit_fmt(egui::Slider::new(&mut self.s.smart_lo, 45.0..=85.0), f));
                    ui.end_row();
                    ui.label(RichText::new("Full speed at").color(DIM));
                    ui.add(unit_fmt(egui::Slider::new(&mut self.s.smart_hi, 60.0..=100.0), f));
                    ui.end_row();
                });

                ui.add_space(10.0);
                ui.label(RichText::new("BACKGROUND HELPER").font(font(10.5)).color(FAINT));
                ui.label(
                    RichText::new("Your mode keeps running after you quit Mac Smart Cooler and after restarts. Choose Apple Default to give the fans back to macOS.")
                        .font(font(12.0))
                        .color(DIM),
                );
                if self.helper != helper::Status::NotInstalled {
                    ui.add_space(4.0);
                    if button(ui, "Remove background helper", false).clicked() {
                        request_uninstall(ctx.clone());
                    }
                }
            });
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.show_prefs = false;
        }
        if self.s.smart_hi < self.s.smart_lo + 5.0 {
            self.s.smart_hi = self.s.smart_lo + 5.0;
        }
        // keep Smart Cooling live while its curve is being edited
        if preset == Preset::Smart && (old_lo != self.s.smart_lo || old_hi != self.s.smart_hi) {
            self.apply_preset(Preset::Smart, snap);
        }
    }
}

/// Opaque equivalent of the hairline colour on a card, for blending.
fn line_solid() -> Color32 {
    Color32::from_rgb(45, 47, 54)
}
