//! Window-only preferences: appearance and zoom.
//!
//! Kept apart from `settings.rs`, which the background helper shares (and whose source is
//! part of the helper's version hash). Stored in
//! ~/Library/Application Support/Mac Smart Cooler/ui.txt as `key=value` lines.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct UiPrefs {
    pub theme: ThemeMode,
    /// Size of the whole interface: 1.0 is 100%.
    pub zoom: f32,
    /// Set by the snapshot walkthrough so it never changes the saved file.
    pub frozen: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self { theme: ThemeMode::System, zoom: 1., frozen: false }
    }
}

fn file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Mac Smart Cooler/ui.txt"))
}

fn parse_theme(value: &str) -> ThemeMode {
    match value.trim() {
        "light" => ThemeMode::Light,
        "dark" => ThemeMode::Dark,
        _ => ThemeMode::System,
    }
}

/// MACSMARTCOOLER_THEME overrides the saved theme for one run (debug builds: snapshots).
pub fn theme_override() -> Option<ThemeMode> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var("MACSMARTCOOLER_THEME").ok().map(|v| parse_theme(&v))
}

/// MACSMARTCOOLER_ZOOM overrides the saved zoom for one run (debug builds: snapshots).
pub fn zoom_override() -> Option<f32> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let zoom: f32 = std::env::var("MACSMARTCOOLER_ZOOM").ok()?.trim().parse().ok()?;
    (0.5..=3.0).contains(&zoom).then_some(zoom)
}

/// The zoom level closest to a stored value, so a hand-edited file can't break the layout.
fn nearest_zoom(value: f32) -> f32 {
    let steps = crate::theme::ZOOM_STEPS;
    steps.into_iter().fold(1., |best, s| if (s - value).abs() < (best - value).abs() { s } else { best })
}

impl UiPrefs {
    pub fn load() -> Self {
        let mut p = Self::parse(&file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default());
        if let Some(t) = theme_override() {
            p.theme = t;
        }
        if let Some(z) = zoom_override() {
            p.zoom = z;
        }
        p
    }

    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key {
                "theme" => s.theme = parse_theme(value),
                "zoom" => s.zoom = value.trim().parse().map(nearest_zoom).unwrap_or(1.),
                _ => {}
            }
        }
        s
    }

    fn serialize(&self) -> String {
        let theme = match self.theme {
            ThemeMode::System => "system",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        };
        format!("theme={theme}\nzoom={}\n", self.zoom)
    }

    pub fn save(&self) {
        if self.frozen {
            return;
        }
        let Some(path) = file() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, self.serialize());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_snaps_to_an_offered_level() {
        assert_eq!(UiPrefs::parse("zoom=1.15\n").zoom, 1.15);
        assert_eq!(UiPrefs::parse("zoom=1.2\n").zoom, 1.15);
        assert_eq!(UiPrefs::parse("zoom=40\n").zoom, 1.5);
        assert_eq!(UiPrefs::parse("zoom=-3\n").zoom, 0.85);
        assert_eq!(UiPrefs::parse("zoom=big\n").zoom, 1.);
    }

    #[test]
    fn round_trip() {
        let p = UiPrefs { theme: ThemeMode::Dark, zoom: 1.3, frozen: false };
        assert_eq!(UiPrefs::parse(&p.serialize()), p);
        assert_eq!(UiPrefs::parse("theme=light\n").theme, ThemeMode::Light);
        assert_eq!(UiPrefs::parse("").theme, ThemeMode::System);
    }
}
