//! One line of text that ends in "…" when it doesn't fit.
//!
//! GPUI 0.2.2's own `text_ellipsis()` shortens the text's style runs in place while measuring,
//! and the layout measures the same text more than once at different widths. A later pass then
//! shapes a longer string with runs that were cut for a shorter one; when the cut falls inside a
//! multi-byte character ("·", "°", "…") it panics. This element shapes and cuts the line itself,
//! from fresh runs each time.

use gpui::{
    px, size, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, ShapedLine, Style, TextRun, Window,
};

pub struct Elided {
    text: SharedString,
}

pub fn elided(text: impl Into<SharedString>) -> Elided {
    Elided { text: text.into() }
}

pub struct Measure {
    font_size: Pixels,
    line_height: Pixels,
    run: TextRun,
}

impl IntoElement for Elided {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn shape(text: &str, m: &Measure, window: &Window) -> ShapedLine {
    let run = TextRun { len: text.len(), ..m.run.clone() };
    window.text_system().shape_line(SharedString::from(text.to_string()), m.font_size, &[run], None)
}

/// The text cut to fit `width`, with "…" at the end (the whole text if it fits).
fn fit(text: &str, width: Pixels, m: &Measure, window: &Window) -> ShapedLine {
    let full = shape(text, m, window);
    if full.width <= width {
        return full;
    }
    // the longest prefix (on a character boundary) that fits with the ellipsis
    let bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let (mut lo, mut hi) = (0usize, bounds.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate = format!("{}…", text[..bounds[mid.min(bounds.len() - 1)]].trim_end());
        if shape(&candidate, m, window).width <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let cut = if lo == 0 { 0 } else { bounds[lo.min(bounds.len() - 1)] };
    shape(&format!("{}…", text[..cut].trim_end()), m, window)
}

impl Element for Elided {
    type RequestLayoutState = Measure;
    type PrepaintState = Option<ShapedLine>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, Measure) {
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = style.line_height.to_pixels(font_size.into(), window.rem_size());
        let m = Measure { font_size, line_height, run: style.to_run(self.text.len()) };
        let natural = shape(&self.text, &m, window).width.ceil();
        let mut layout = Style::default();
        layout.min_size.width = px(0.).into();
        layout.flex_shrink = 1.;
        let id = window.request_measured_layout(layout, move |known, available, _, _| {
            let width = known.width.unwrap_or(match available.width {
                AvailableSpace::Definite(w) => natural.min(w),
                AvailableSpace::MinContent => px(0.),
                AvailableSpace::MaxContent => natural,
            });
            size(width, line_height)
        });
        (id, m)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        m: &mut Measure,
        window: &mut Window,
        _: &mut App,
    ) -> Option<ShapedLine> {
        // a hair of slack so a line measured at exactly its width is never cut
        Some(fit(&self.text, bounds.size.width + px(0.5), m, window))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        m: &mut Measure,
        line: &mut Option<ShapedLine>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(line) = line.take() {
            let _ = line.paint(bounds.origin, m.line_height, window, cx);
        }
    }
}
