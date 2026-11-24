//! Building blocks. Most are Mac Cleaner's (`button`, `badge`, `tile`, `modal_frame`,
//! `icon`, `spinner`, shadows, `Ellipsis`), copied so both apps look the same; the
//! segmented control, switch, slider, stepper, status pill and tooltip are new, in the same style.

use crate::theme::*;
use gpui::{
    div, linear_color_stop, linear_gradient, percentage, point, prelude::*, relative, rgb, svg, white, Animation,
    AnimationExt as _, AnyView, App, BoxShadow, Div, ElementId, FontWeight, Hsla, SharedString, Stateful,
    Transformation, Window,
};
use std::time::Duration;

/// A one-line box for an `elided()` text, which ends in "…" when it doesn't fit.
/// (Mac Cleaner's version used GPUI's `text_ellipsis()`; see elided.rs for why this doesn't.)
pub trait Ellipsis: Styled + Sized {
    fn ellipsis(self) -> Self {
        self.overflow_hidden().whitespace_nowrap()
    }
}
impl<T: Styled> Ellipsis for T {}

pub fn icon(name: &str, size: f32, color: impl Into<Hsla>) -> gpui::Svg {
    svg().path(format!("icons/{name}.svg")).size(pt(size)).flex_none().text_color(color.into())
}

pub fn spinner(id: impl Into<ElementId>, size: f32, t: &Theme) -> Div {
    div()
        .relative()
        .size(pt(size))
        .flex_none()
        .child(div().absolute().inset_0().rounded_full().border(pt(2.)).border_color(t.border_strong))
        .child(
            svg()
                .absolute()
                .inset_0()
                .size(pt(size))
                .path("icons/spinner.svg")
                .text_color(t.accent)
                .with_animation(id, Animation::new(Duration::from_millis(800)).repeat(), |s, delta| {
                    s.with_transformation(Transformation::rotate(percentage(delta)))
                }),
        )
}

pub fn gradient(top: u32, bottom: u32) -> gpui::Background {
    linear_gradient(180., linear_color_stop(rgb(top), 0.), linear_color_stop(rgb(bottom), 1.))
}

pub fn gradient_h(left: Hsla, right: Hsla) -> gpui::Background {
    linear_gradient(90., linear_color_stop(left, 0.), linear_color_stop(right, 1.))
}

pub fn tile(id: &str, icon_name: &str, size: f32) -> Div {
    let (top, bottom) = if id == "muted" { (0xb5b5bb, 0x8e8e93) } else { tile_colors(id) };
    div()
        .size(pt(size))
        .flex_none()
        .rounded(pt(size * 0.27))
        .flex()
        .items_center()
        .justify_center()
        .bg(gradient(top, bottom))
        .shadow(vec![shadow(rgba_hsla(0x00000014), 1., 2., 1.)])
        .child(icon(icon_name, size * 0.57, white()))
}

pub fn tile_lg(id: &str, icon_name: &str) -> Div {
    tile(id, icon_name, 44.).rounded(pt(11.))
}

pub fn badge(label: impl Into<SharedString>, fg: Hsla, bg: Hsla) -> Div {
    div()
        .flex_none()
        .h(pt(20.))
        .px(pt(8.))
        .rounded(pt(10.))
        .flex()
        .items_center()
        .bg(bg)
        .text_color(fg)
        .text_size(pt(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .whitespace_nowrap()
        .child(label.into())
}

/// A badge with a status dot in front ("● Fan control on").
pub fn status_pill(label: impl Into<SharedString>, fg: Hsla, bg: Hsla) -> Div {
    badge(label, fg, bg).h(pt(22.)).px(pt(9.)).gap(pt(6.)).child(div().size(pt(6.)).rounded_full().bg(fg)).flex_row_reverse()
}

/// Small grey section heading ("MODES", "FANS").
pub fn section(label: impl Into<SharedString>, t: &Theme) -> Div {
    div().text_size(pt(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.text_3).child(label.into())
}

#[derive(Clone, Copy, PartialEq)]
pub enum Btn {
    Primary,
    Secondary,
    Ghost,
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, icon_name: Option<&str>, kind: Btn, enabled: bool, t: &Theme) -> Stateful<Div> {
    let (bg, hover_bg, fg) = match kind {
        Btn::Primary => (t.accent, t.accent_hover, white()),
        Btn::Secondary => (t.surface, t.surface_2, t.text),
        Btn::Ghost => (gpui::transparent_black(), t.accent_soft, t.accent),
    };
    let el = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(pt(6.))
        .whitespace_nowrap()
        .bg(bg)
        .text_color(fg)
        .font_weight(FontWeight::MEDIUM)
        .when(kind == Btn::Ghost, |b| b.h(pt(28.)).px(pt(10.)).rounded(pt(8.)))
        .when(kind != Btn::Ghost, |b| b.h(pt(32.)).px(pt(14.)).rounded(pt(8.)))
        .when(kind == Btn::Secondary, |b| b.border_1().border_color(t.border_strong))
        .when(kind != Btn::Ghost, |b| b.shadow(small_shadow(t)))
        .when_some(icon_name, |b, name| b.child(icon(name, 15., fg)))
        .child(label.into());
    if enabled {
        el.cursor_pointer().hover(move |s| s.bg(hover_bg))
    } else {
        el.opacity(0.4)
    }
}

/// The dialog card. `render` places it on a dimmed backdrop.
pub fn modal_frame(t: &Theme, width: gpui::Rems, max_h: impl Clone + Into<gpui::Length>) -> Stateful<Div> {
    div()
        .id("modal-card")
        .w(width)
        .max_h(max_h)
        .flex()
        .flex_col()
        .rounded(pt(16.))
        .bg(t.surface)
        .shadow(vec![shadow(rgba_hsla(0x00000038), 24., 64., t.scale)])
        .border_1()
        .border_color(t.border)
        .overflow_hidden()
        // Clicks inside the card must not reach the backdrop (which cancels).
        .on_click(|_, _, cx| cx.stop_propagation())
}

/// A raised card, like Mac Cleaner's "Selected to clean" card.
pub fn card(t: &Theme) -> Div {
    div().rounded(pt(14.)).bg(t.surface).shadow(card_shadow(t))
}

/// The look of one segment row (Mac Cleaner's zoom control). Each segment is added by the
/// caller with `segment`, so it can carry its own click handler.
pub fn segmented(t: &Theme) -> Div {
    div().flex().items_center().gap(pt(2.)).p(pt(2.)).rounded(pt(8.)).bg(t.border)
}

pub fn segment(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool, t: &Theme) -> Stateful<Div> {
    let hover = t.hover;
    div()
        .id(id)
        .flex_1()
        .h(pt(24.))
        .px(pt(10.))
        .rounded(pt(6.))
        .flex()
        .items_center()
        .justify_center()
        .whitespace_nowrap()
        .text_size(pt(12.))
        .font_weight(FontWeight::MEDIUM)
        .when(selected, |el| el.bg(t.surface).text_color(t.text).shadow(small_shadow(t)))
        .when(!selected, |el| el.text_color(t.text_2).cursor_pointer().hover(move |s| s.bg(hover)))
        .child(label.into())
}

/// On/off switch. The caller adds the click handler.
pub fn switch(id: impl Into<ElementId>, on: bool, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .w(pt(38.))
        .h(pt(22.))
        .flex_none()
        .p(pt(2.))
        .rounded_full()
        .flex()
        .items_center()
        .when(on, |el| el.bg(t.accent).justify_end())
        .when(!on, |el| el.bg(t.border_strong).justify_start())
        .cursor_pointer()
        .child(div().size(pt(18.)).rounded_full().bg(white()).shadow(small_shadow(t)))
}

/// Slider look: a track with the filled part and a round knob at `frac` (0..1).
/// The parent view handles the mouse (it knows the track's bounds).
pub fn slider_track(frac: f32, t: &Theme) -> Div {
    let frac = frac.clamp(0., 1.);
    div()
        .relative()
        .h(pt(22.))
        .flex()
        .items_center()
        .child(
            div()
                .w_full()
                .h(pt(4.))
                .rounded(pt(2.))
                .bg(t.border_strong)
                .child(div().h_full().w(relative(frac)).rounded(pt(2.)).bg(t.accent)),
        )
        .child(
            // the knob is centred on the value: shifted left by half its width
            div()
                .absolute()
                .top(pt(2.))
                .left(relative(frac))
                .ml(pt(-9.))
                .size(pt(18.))
                .rounded_full()
                .bg(white())
                .border_1()
                .border_color(t.border_strong)
                .shadow(small_shadow(t)),
        )
}

/// A − or + button of a number stepper.
pub fn step_button(id: impl Into<ElementId>, icon_name: &str, enabled: bool, t: &Theme) -> Stateful<Div> {
    let hover = t.hover;
    div()
        .id(id)
        .w(pt(26.))
        .h(pt(24.))
        .rounded(pt(6.))
        .flex()
        .items_center()
        .justify_center()
        .child(icon(icon_name, 13., t.text_2))
        .when(!enabled, |el| el.opacity(0.35))
        .when(enabled, |el| el.cursor_pointer().hover(move |s| s.bg(hover)))
}

/// Hover text, in the app's card style.
pub struct Tip {
    text: SharedString,
    t: Theme,
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = &self.t;
        div()
            .max_w(pt(280.))
            .px(pt(9.))
            .py(pt(5.))
            .rounded(pt(7.))
            .bg(t.surface)
            .border_1()
            .border_color(t.border_strong)
            .shadow(vec![shadow(rgba_hsla(0x00000030), 4., 12., t.scale)])
            .font_family(".SystemUIFont")
            .text_size(pt(12.))
            .line_height(relative(1.35))
            .text_color(t.text)
            .child(self.text.clone())
    }
}

pub fn tip(text: impl Into<SharedString>, t: &Theme) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let (text, t) = (text.into(), *t);
    move |_, cx| cx.new(|_| Tip { text: text.clone(), t }).into()
}

/// Shadows are the one thing GPUI wants in real pixels, so they are scaled by hand.
pub fn shadow(color: Hsla, y: f32, blur: f32, scale: f32) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(gpui::px(0.), gpui::px(y * scale)),
        blur_radius: gpui::px(blur * scale),
        spread_radius: gpui::px(0.),
    }
}

pub fn card_shadow(t: &Theme) -> Vec<BoxShadow> {
    vec![shadow(t.shadow.opacity(0.5), 1., 2., t.scale), shadow(t.shadow.opacity(0.6), 4., 14., t.scale)]
}

pub fn small_shadow(t: &Theme) -> Vec<BoxShadow> {
    vec![shadow(t.shadow.opacity(0.6), 1., 2., t.scale)]
}

pub fn rgba_hsla(hex: u32) -> Hsla {
    gpui::rgba(hex).into()
}
