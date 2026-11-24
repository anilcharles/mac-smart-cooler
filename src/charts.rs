//! The two drawn elements: the hottest-core ring gauge and the fan rpm history.
//! Sizes are given in points and multiplied by the zoom (`scale`), since painting is in pixels.

use crate::theme::{mix, Theme};
use gpui::{
    canvas, fill, linear_color_stop, linear_gradient, point, prelude::*, px, Bounds, Hsla, PathBuilder, Pixels,
    Point, Window,
};
use std::collections::VecDeque;
use std::f32::consts::PI;

fn dot(window: &mut Window, c: Point<Pixels>, r: f32, color: Hsla) {
    let b = Bounds { origin: point(c.x - px(r), c.y - px(r)), size: gpui::size(px(2. * r), px(2. * r)) };
    window.paint_quad(fill(b, color).corner_radii(px(r)));
}

/// Stroke an arc (screen angles, clockwise from `a0` to `a1`) in short pieces, each in the
/// colour `color(u)` for its position `u` along the arc; round ends.
fn arc(window: &mut Window, c: Point<Pixels>, r: f32, a0: f32, a1: f32, width: f32, color: impl Fn(f32) -> Hsla) {
    let at = |a: f32| point(c.x + px(r * a.cos()), c.y + px(r * a.sin()));
    let steps = (((a1 - a0).abs() / (2. * PI)) * 96.).ceil().max(2.) as usize;
    for i in 0..steps {
        let (u0, u1) = (i as f32 / steps as f32, (i + 1) as f32 / steps as f32);
        // overlap pieces a little so no hairline gaps show between them
        let mut p = PathBuilder::stroke(px(width));
        p.move_to(at(a0 + (a1 - a0) * u0));
        p.line_to(at(a0 + (a1 - a0) * (u1 + 0.3 / steps as f32).min(1.)));
        if let Ok(path) = p.build() {
            window.paint_path(path, color((u0 + u1) / 2.));
        }
    }
    dot(window, at(a0), width / 2., color(0.));
    dot(window, at(a1), width / 2., color(1.));
}

/// Three-quarter ring: the track, then the hottest-core temperature (30..100 °C) in the
/// temperature colours.
pub fn ring_gauge(celsius: Option<f64>, t: Theme) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let s = t.scale;
            let width = 8. * s;
            let c = bounds.center();
            let r = f32::from(bounds.size.width.min(bounds.size.height)) / 2. - width / 2. - 1.;
            let (a0, a1) = (PI * 0.75, PI * 2.25);
            let track = if t.dark { mix(t.surface, t.text, 0.12) } else { mix(t.surface, t.text, 0.09) };
            arc(window, c, r, a0, a1, width, |_| track);
            if let Some(v) = celsius {
                let frac = ((v as f32 - 30.) / 70.).clamp(0.02, 1.);
                arc(window, c, r, a0, a0 + (a1 - a0) * frac, width, |u| t.temp(30. + (u * frac * 70.) as f64));
            }
        },
    )
    .size_full()
}

/// Area chart of the last `slots` values (newest on the right), scaled 0..max.
pub fn sparkline(values: VecDeque<f32>, slots: usize, max: f32, t: Theme) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let s = t.scale;
            let (x0, y0) = (f32::from(b.origin.x), f32::from(b.origin.y));
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
            // faint guide lines
            for k in 1..4 {
                let y = y0 + h * k as f32 / 4.;
                let line = Bounds { origin: point(px(x0), px(y.round())), size: gpui::size(px(w), px(1.)) };
                window.paint_quad(fill(line, t.border.opacity(0.6)));
            }
            if values.len() < 2 || max <= 0. {
                return;
            }
            let n = slots.max(2) as f32 - 1.;
            let start = slots.saturating_sub(values.len());
            let pts: Vec<Point<Pixels>> = values
                .iter()
                .enumerate()
                .map(|(i, &v)| point(px(x0 + w * (start + i) as f32 / n), px(y0 + h - h * (v / max).clamp(0., 1.))))
                .collect();
            let bottom = px(y0 + h);
            let mut area = PathBuilder::fill();
            area.move_to(point(pts[0].x, bottom));
            for p in &pts {
                area.line_to(*p);
            }
            area.line_to(point(pts[pts.len() - 1].x, bottom));
            area.close();
            if let Ok(path) = area.build() {
                let a = if t.dark { 0.32 } else { 0.22 };
                window.paint_path(
                    path,
                    linear_gradient(180., linear_color_stop(t.accent.opacity(a), 0.), linear_color_stop(t.accent.opacity(0.), 1.)),
                );
            }
            let mut line = PathBuilder::stroke(px(1.6 * s));
            line.move_to(pts[0]);
            for p in &pts[1..] {
                line.line_to(*p);
            }
            if let Ok(path) = line.build() {
                window.paint_path(path, t.cyan);
            }
            if values.back().is_some_and(|&v| v > 0.) {
                let last = pts[pts.len() - 1];
                dot(window, last, 5.5 * s, t.cyan.opacity(0.25));
                dot(window, last, 3. * s, t.cyan);
            }
        },
    )
    .size_full()
}
