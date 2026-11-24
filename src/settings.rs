//! Settings shared by the app and the background helper.
//!
//! The system copy lives in a root-owned folder and is written only by root (the helper,
//! or the app when run with sudo), so no other account can tamper with it or use it to
//! trick root into writing somewhere else. The app asks the helper to save; without a
//! helper it keeps a private copy in the user's own Library instead.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

const DIR: &str = "/Library/Application Support/MacSmartCooler";
const FILE: &str = "/Library/Application Support/MacSmartCooler/settings.txt";
/// Where earlier versions kept settings (a folder every account can write to). Root never
/// reads these; the app reads one only if this user owns it and nobody else can write it.
const LEGACY: [&str; 2] = [
    "/Users/Shared/MacSmartCooler/settings.txt",
    "/Users/Shared/MacBreeze/settings.txt",
];

#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    pub preset: String,
    pub smart_lo: f64,
    pub smart_hi: f64,
    pub fahrenheit: bool,
    pub show_unnamed: bool,
    /// Each fan's exact mode as the helper last applied it ("smart,60,88|const,3000"),
    /// so any mode, including Custom, comes back after a restart. Written only by the
    /// helper; empty when unknown.
    pub fan_modes: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            preset: "auto".into(),
            smart_lo: 60.0,
            smart_hi: 88.0,
            fahrenheit: false,
            show_unnamed: false,
            fan_modes: String::new(),
        }
    }
}

/// The user's private copy, used when no helper is installed.
fn user_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/MacSmartCooler/settings.txt"))
}

/// Parse "key=value" pairs separated by newlines or ';', validating every value.
pub fn parse(text: &str) -> Settings {
    let mut s = Settings::default();
    for item in text.split(['\n', ';']) {
        let Some((k, v)) = item.split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "preset" if matches!(v, "auto" | "smart" | "full" | "custom") => s.preset = v.to_string(),
            "cool_from" => s.smart_lo = v.parse().unwrap_or(s.smart_lo),
            "cool_max" => s.smart_hi = v.parse().unwrap_or(s.smart_hi),
            "fahrenheit" => s.fahrenheit = v == "true",
            "show_unnamed" => s.show_unnamed = v == "true",
            "fan_modes" if valid_fan_modes(v) => s.fan_modes = v.to_string(),
            _ => {}
        }
    }
    (s.smart_lo, s.smart_hi) = clamp_smart(s.smart_lo, s.smart_hi);
    s
}

/// Every entry must decode as a fan mode (the same checks as socket commands).
fn valid_fan_modes(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 1024
        && v.split('|').count() <= 16
        && v.split('|').all(|m| crate::worker::FanMode::decode(m).is_some())
}

/// Smart Cooling's two chip temperatures, forced into their allowed ranges.
pub fn clamp_smart(lo: f64, hi: f64) -> (f64, f64) {
    let lo = if lo.is_finite() { lo.clamp(45.0, 85.0) } else { 60.0 };
    let hi = if hi.is_finite() { hi } else { 88.0 };
    (lo, hi.clamp(lo + 5.0, 100.0))
}

/// One line, safe to send to the helper.
pub fn encode(s: &Settings) -> String {
    let mut out = format!(
        "preset={};cool_from={};cool_max={};fahrenheit={};show_unnamed={}",
        s.preset, s.smart_lo, s.smart_hi, s.fahrenheit, s.show_unnamed
    );
    if !s.fan_modes.is_empty() {
        out.push_str(&format!(";fan_modes={}", s.fan_modes));
    }
    out
}

pub fn load() -> Settings {
    let me = unsafe { geteuid() };
    // root (the helper, or sudo) trusts only the root-owned system copy
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from(FILE)];
    if me != 0 {
        candidates.extend(user_file());
        candidates.extend(LEGACY.iter().map(PathBuf::from));
    }
    for p in candidates {
        let owner = if p == PathBuf::from(FILE) { 0 } else { me };
        if let Some(text) = read_trusted(&p, owner) {
            return parse(&text);
        }
    }
    Settings::default()
}

extern "C" {
    fn geteuid() -> u32;
}

const MAX_SETTINGS_BYTES: u64 = 4096;

/// Read a small settings file only if it is a regular file (no symlink, FIFO or device),
/// owned by `owner`, and not writable by group or others. Reads at most 4 KiB.
fn read_trusted(path: &PathBuf, owner: u32) -> Option<String> {
    use std::io::Read;
    let f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x0100 | 0x0004) // O_NOFOLLOW | O_NONBLOCK (a FIFO can't stall us)
        .open(path)
        .ok()?;
    let m = f.metadata().ok()?;
    if !m.file_type().is_file() || m.uid() != owner || m.mode() & 0o022 != 0 {
        return None;
    }
    let mut text = String::new();
    f.take(MAX_SETTINGS_BYTES).read_to_string(&mut text).ok()?;
    Some(text)
}

pub fn save(s: &Settings) {
    if crate::smc::is_root() {
        let _ = save_system(s);
    } else if crate::helper::rpc(&format!("save {}", encode(s))).as_deref() != Ok("ok") {
        // no helper: keep a private copy in the user's own Library
        if let Some(f) = user_file() {
            let _ = f.parent().map(fs::create_dir_all);
            let _ = write_new(&f, &encode(s).replace(';', "\n"), 0o600);
        }
    }
}

/// Root only: write the system copy. Refuses a folder that isn't a real, root-owned
/// directory, and never follows a symlink planted in place of the file.
pub fn save_system(s: &Settings) -> Result<(), String> {
    match fs::symlink_metadata(DIR) {
        Ok(m) if m.is_dir() && m.uid() == 0 && m.mode() & 0o022 == 0 => {}
        Ok(_) => return Err(format!("{DIR} is not a root-owned folder; refusing to write")),
        Err(_) => {
            fs::create_dir_all(DIR).map_err(|e| e.to_string())?;
            fs::set_permissions(DIR, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
    }
    write_new(&PathBuf::from(FILE), &encode(s).replace(';', "\n"), 0o644)
}

/// Write to a fresh temp file (never following links) and rename it over the target.
fn write_new(path: &PathBuf, body: &str, mode: u32) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true) // fails on an existing file or symlink
        .mode(mode)
        .custom_flags(0x0100) // O_NOFOLLOW
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    f.write_all(format!("{body}\n").as_bytes()).map_err(|e| e.to_string())?;
    drop(f);
    // rename replaces a symlink at the target rather than writing through it
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_bad_values() {
        let s = parse("preset=../../etc;cool_from=NaN;cool_max=inf;fahrenheit=true");
        assert_eq!(s.preset, "auto");
        assert_eq!((s.smart_lo, s.smart_hi), (60.0, 88.0));
        assert!(s.fahrenheit);
        let s = parse(&encode(&Settings { preset: "smart".into(), smart_lo: 10.0, smart_hi: 500.0, ..Default::default() }));
        assert_eq!((s.preset.as_str(), s.smart_lo, s.smart_hi), ("smart", 45.0, 100.0));
    }

    #[test]
    fn fan_modes_round_trip_and_reject_garbage() {
        let s = Settings { fan_modes: "smart,60,88|const,3000|sensor,max:hot,45,80".into(), ..Default::default() };
        assert_eq!(parse(&encode(&s)).fan_modes, s.fan_modes);
        for bad in ["const,NaN", "smart,60|auto", "rm -rf /", "auto|auto|auto|auto|auto|auto|auto|auto|auto|auto|auto|auto|auto|auto|auto|auto|auto"] {
            assert_eq!(parse(&format!("fan_modes={bad}")).fan_modes, "", "{bad}");
        }
    }

    #[test]
    fn untrusted_settings_files_are_ignored() {
        let dir = std::env::temp_dir().join(format!("msc-read-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let me = unsafe { geteuid() };
        let good = dir.join("good.txt");
        fs::write(&good, "preset=smart").unwrap();
        fs::set_permissions(&good, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read_trusted(&good, me).as_deref(), Some("preset=smart"));
        // owned by someone else
        assert_eq!(read_trusted(&good, me + 1), None);
        // writable by other accounts
        let open = dir.join("open.txt");
        fs::write(&open, "preset=full").unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(read_trusted(&open, me), None);
        // a symlink, even to a good file
        let link = dir.join("link.txt");
        std::os::unix::fs::symlink(&good, &link).unwrap();
        assert_eq!(read_trusted(&link, me), None);
        // a huge file is cut at 4 KiB
        let big = dir.join("big.txt");
        fs::write(&big, vec![b'x'; 100_000]).unwrap();
        fs::set_permissions(&big, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read_trusted(&big, me).map(|t| t.len()), Some(4096));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_new_replaces_a_planted_symlink_instead_of_following_it() {
        let dir = std::env::temp_dir().join(format!("msc-set-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim");
        fs::write(&victim, "untouched").unwrap();
        let target = dir.join("settings.txt");
        std::os::unix::fs::symlink(&victim, &target).unwrap();
        write_new(&target, "preset=smart", 0o644).unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");
        assert!(!fs::symlink_metadata(&target).unwrap().file_type().is_symlink());
        let _ = fs::remove_dir_all(&dir);
    }
}
