//! Colours and metrics. The palette and `pt()` are Mac Cleaner's, so both apps look alike;
//! the temperature scale and fan-gauge colours are this app's own.

use gpui::{rgb, rgba, Hsla, Rems, WindowAppearance};

/// Mac Cleaner's palette, kept whole (a few colours are unused here).
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct Theme {
    /// The zoom factor, for the few sizes that must be given in real pixels (shadows).
    pub scale: f32,
    pub bg: Hsla,
    pub sidebar: Hsla,
    pub surface: Hsla,
    pub surface_2: Hsla,
    pub hover: Hsla,
    pub selected_row: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub text_2: Hsla,
    pub text_3: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_soft: Hsla,
    pub danger: Hsla,
    pub danger_hover: Hsla,
    pub danger_soft: Hsla,
    pub success: Hsla,
    pub success_soft: Hsla,
    pub warning: Hsla,
    pub warning_soft: Hsla,
    pub folder: Hsla,
    pub shadow: Hsla,
    pub backdrop: Hsla,
    /// Temperature scale: cool -> warm -> hot -> red (darker in light mode, to read on white).
    pub t_cool: Hsla,
    pub t_warm: Hsla,
    pub t_hot: Hsla,
    pub t_red: Hsla,
    /// Second colour of the fan gauge (the first is `accent`).
    pub cyan: Hsla,
    /// Light theme or not, for the few colours mixed by hand.
    pub dark: bool,
}

impl Theme {
    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            _ => Self::light(),
        }
    }

    pub fn light() -> Self {
        Self {
            scale: 1.,
            bg: rgb(0xf5f5f7).into(),
            sidebar: rgb(0xececef).into(),
            surface: rgb(0xffffff).into(),
            surface_2: rgb(0xf7f7f9).into(),
            hover: rgba(0x00000009).into(),
            selected_row: rgba(0x007aff10).into(),
            border: rgba(0x00000014).into(),
            border_strong: rgba(0x00000024).into(),
            text: rgb(0x1d1d1f).into(),
            text_2: rgb(0x6e6e73).into(),
            text_3: rgb(0x8e8e93).into(),
            accent: rgb(0x007aff).into(),
            accent_hover: rgb(0x0068d9).into(),
            accent_soft: rgba(0x007aff1a).into(),
            danger: rgb(0xff3b30).into(),
            danger_hover: rgb(0xe0332a).into(),
            danger_soft: rgba(0xff3b301f).into(),
            success: rgb(0x34c759).into(),
            success_soft: rgba(0x34c7591f).into(),
            warning: rgb(0xf09000).into(),
            warning_soft: rgba(0xff9f0a1f).into(),
            folder: rgb(0x3a9cf5).into(),
            shadow: rgba(0x0000001a).into(),
            backdrop: rgba(0x00000047).into(),
            t_cool: rgb(0x1aa57d).into(),
            t_warm: rgb(0xd99a00).into(),
            t_hot: rgb(0xf06a1c).into(),
            t_red: rgb(0xff3b30).into(),
            cyan: rgb(0x32ade6).into(),
            dark: false,
        }
    }

    pub fn dark() -> Self {
        Self {
            scale: 1.,
            bg: rgb(0x1c1c1e).into(),
            sidebar: rgb(0x232325).into(),
            surface: rgb(0x2a2a2d).into(),
            surface_2: rgb(0x303033).into(),
            hover: rgba(0xffffff0a).into(),
            selected_row: rgba(0x0a84ff1a).into(),
            border: rgba(0xffffff14).into(),
            border_strong: rgba(0xffffff29).into(),
            text: rgb(0xf5f5f7).into(),
            text_2: rgb(0xa1a1a6).into(),
            text_3: rgb(0x7c7c80).into(),
            accent: rgb(0x0a84ff).into(),
            accent_hover: rgb(0x3395ff).into(),
            accent_soft: rgba(0x0a84ff2e).into(),
            danger: rgb(0xff453a).into(),
            danger_hover: rgb(0xff5e55).into(),
            danger_soft: rgba(0xff453a29).into(),
            success: rgb(0x30d158).into(),
            success_soft: rgba(0x30d15826).into(),
            warning: rgb(0xffa526).into(),
            warning_soft: rgba(0xffa52626).into(),
            folder: rgb(0x4aa8ff).into(),
            shadow: rgba(0x00000066).into(),
            backdrop: rgba(0x00000073).into(),
            t_cool: rgb(0x30d1a0).into(),
            t_warm: rgb(0xffc440).into(),
            t_hot: rgb(0xff8040).into(),
            t_red: rgb(0xff453a).into(),
            cyan: rgb(0x64d2ff).into(),
            dark: true,
        }
    }
    /// Colour of a temperature in °C on the continuous scale.
    pub fn temp(&self, c: f64) -> Hsla {
        let c = c as f32;
        if c < 45. {
            self.t_cool
        } else if c < 65. {
            mix(self.t_cool, self.t_warm, (c - 45.) / 20.)
        } else if c < 82. {
            mix(self.t_warm, self.t_hot, (c - 65.) / 17.)
        } else {
            mix(self.t_hot, self.t_red, (c - 82.) / 13.)
        }
    }

    /// Colour of a load fraction (0..1), on the same scale.
    pub fn load(&self, f: f64) -> Hsla {
        self.temp(40. + f * 45.)
    }
}

/// Blend two colours in RGB, `t` in 0..1.
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b, t) = (a.to_rgb(), b.to_rgb(), t.clamp(0., 1.));
    let l = |x: f32, y: f32| x + (y - x) * t;
    gpui::Rgba { r: l(a.r, b.r), g: l(a.g, b.g), b: l(a.b, b.b), a: l(a.a, b.a) }.into()
}

/// Gradient colours of the mode tiles (top, bottom).
pub fn tile_colors(id: &str) -> (u32, u32) {
    match id {
        "auto" => (0xa7a7b0, 0x74747c),
        "smart" => (0x64d8cb, 0x2ba9bf),
        "full" => (0xffbb4d, 0xff8c00),
        "custom" => (0xd98af5, 0xa64bd6),
        "fan" => (0x6fb1ff, 0x3a6fe0),
        "settings" => (0x9fabbd, 0x5f6d82),
        _ => (0x5ac8fa, 0x007aff),
    }
}

/// Zoom levels offered by View > Zoom In / Zoom Out.
pub const ZOOM_STEPS: [f32; 5] = [0.85, 1.0, 1.15, 1.3, 1.5];

/// Window size at 100% zoom, in points: (default, minimum).
pub const WINDOW_SIZE: (f32, f32) = (1120., 740.);
pub const WINDOW_MIN: (f32, f32) = (940., 640.);

/// A size in design points (what it measures at 100% zoom).
///
/// Every size in the UI goes through this, so it is stored in rems and the whole
/// interface scales together when the zoom changes the window's rem size.
pub const fn pt(points: f32) -> Rems {
    Rems(points / 16.)
}

pub const TITLEBAR: Rems = pt(52.);
pub const SIDEBAR_W: Rems = pt(324.);
