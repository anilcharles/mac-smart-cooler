//! Fan reading and control on top of the SMC.

use crate::smc::{Res, Smc, SmcIo};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct FanInfo {
    pub index: usize,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub actual: f64,
    pub target: f64,
    pub manual: bool,
}

pub fn fan_count(smc: &mut dyn SmcIo) -> usize {
    smc.read_f64("FNum").unwrap_or(0.0) as usize
}

fn fan_name(smc: &mut dyn SmcIo, i: usize, count: usize) -> String {
    // Intel Macs store a name in F<i>ID (bytes 4..16).
    if let Some(bytes) = smc.read_bytes(&format!("F{i}ID")) {
        if bytes.len() > 4 {
            let s: String = bytes[4..]
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as char)
                .collect();
            let s = s.trim();
            if !s.is_empty() && s.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                return s.to_string();
            }
        }
    }
    match (count, i) {
        (2, 0) => "Left side".into(),
        (2, 1) => "Right side".into(),
        (1, _) => "Fan".into(),
        _ => format!("Fan {}", i + 1),
    }
}

pub fn read_fan(smc: &mut dyn SmcIo, i: usize, count: usize) -> FanInfo {
    let get = |smc: &mut dyn SmcIo, k: &str| smc.read_f64(&format!("F{i}{k}")).unwrap_or(0.0);
    FanInfo {
        index: i,
        name: fan_name(smc, i, count),
        min: get(smc, "Mn"),
        max: get(smc, "Mx"),
        actual: get(smc, "Ac").max(0.0),
        target: get(smc, "Tg"),
        manual: is_manual(smc, i),
    }
}

/// The per-fan manual-mode key. Most Apple silicon Macs use "F0Md"; some newer ones use
/// lowercase "F0md" (as handled by the Stats app). None on Intel Macs (they use "FS! ").
fn mode_key(smc: &mut dyn SmcIo, i: usize) -> Option<String> {
    for k in [format!("F{i}Md"), format!("F{i}md")] {
        if smc.has_key(&k) {
            return Some(k);
        }
    }
    None
}

fn is_manual(smc: &mut dyn SmcIo, i: usize) -> bool {
    if let Some(md) = mode_key(smc, i) {
        return smc.read_f64(&md) == Some(1.0);
    }
    if let Some(mask) = smc.read_f64("FS! ") {
        return i < 16 && (mask as u32) & (1 << i) != 0;
    }
    false
}

/// Pause between mode writes while waiting for macOS to hand a fan over.
const RETRY_PAUSE: Duration = Duration::from_millis(if cfg!(test) { 1 } else { 100 });

/// Set once this process has put any fan into manual mode.
static TOUCHED: AtomicBool = AtomicBool::new(false);

/// Hand all fans back to macOS, but only if this process ever took control of them.
pub fn restore_if_touched() {
    if TOUCHED.load(Ordering::SeqCst) {
        if let Ok(mut s) = Smc::open() {
            restore_all(&mut s);
        }
    }
}

/// Force fan i into manual mode at the given rpm. Requires root.
pub fn set_manual(smc: &mut dyn SmcIo, i: usize, rpm: f64) -> Res<()> {
    if !rpm.is_finite() || rpm < 0.0 {
        return Err(format!("fan {i}: refusing invalid speed {rpm}"));
    }
    if i >= 16 {
        return Err(format!("fan {i}: no such fan"));
    }
    TOUCHED.store(true, Ordering::SeqCst);
    if let Some(md) = mode_key(smc, i) {
        if !is_manual(smc, i) {
            take_manual(smc, i, &md)?;
        }
    } else if let Some(mask) = smc.read_f64("FS! ") {
        let m = (mask as u16) | (1 << i);
        smc.write_bytes("FS! ", &m.to_be_bytes())?;
    }
    smc.write_f64(&format!("F{i}Tg"), rpm)
}

/// Switch an Apple silicon fan to manual. macOS's thermal service doesn't let go at once:
/// keep writing the mode until it sticks. Macs that have an "Ftst" key (M1-M4 on some macOS
/// versions) also need it set to 1 first; others (e.g. an M1 Max on macOS 27, and M5) don't
/// have it and just need the retries. Verified on an M1 Max with no Ftst key.
fn take_manual(smc: &mut dyn SmcIo, i: usize, md: &str) -> Res<()> {
    let try_mode = |smc: &mut dyn SmcIo| -> Res<bool> {
        match smc.write_f64(md, 1.0) {
            Ok(()) => Ok(is_manual(smc, i)),
            Err(e) if e.contains("not privileged") => Err(e),
            Err(_) => Ok(false),
        }
    };
    if try_mode(smc)? {
        return Ok(());
    }
    if smc.has_key("Ftst") {
        smc.write_f64("Ftst", 1.0)?;
    }
    // up to ~20 s: usually a few seconds
    for _ in 0..200 {
        sleep(RETRY_PAUSE);
        if try_mode(smc)? {
            return Ok(());
        }
    }
    Err(format!("fan {i}: macOS didn't hand over fan control"))
}

/// Return fan i to system (automatic) control.
pub fn set_auto(smc: &mut dyn SmcIo, i: usize) -> Res<()> {
    if let Some(md) = mode_key(smc, i) {
        smc.write_f64(&md, 0.0)?;
        // clear our target so macOS starts from its own speed (as Stats does)
        let _ = smc.write_f64(&format!("F{i}Tg"), 0.0);
    } else if let Some(mask) = smc.read_f64("FS! ") {
        if i < 16 {
            let m = (mask as u16) & !(1 << i);
            smc.write_bytes("FS! ", &m.to_be_bytes())?;
        }
    }
    let n = fan_count(smc);
    if (0..n).all(|j| !is_manual(smc, j)) && smc.has_key("Ftst") {
        smc.write_f64("Ftst", 0.0)?;
    }
    Ok(())
}

/// Give back to macOS any fan still in manual mode, and clear a leftover "Ftst" unlock.
/// Run when the helper starts: if a previous run was killed or crashed, no exit handler
/// ran and the fans may still be held at its last speed. Returns how many were released.
pub fn release_stale(smc: &mut dyn SmcIo) -> usize {
    let n = fan_count(smc);
    let mut released = 0;
    for i in 0..n {
        if is_manual(smc, i) && set_auto(smc, i).is_ok() {
            released += 1;
        }
    }
    if smc.has_key("Ftst") && smc.read_f64("Ftst") == Some(1.0) && (0..n).all(|j| !is_manual(smc, j)) {
        let _ = smc.write_f64("Ftst", 0.0);
    }
    released
}

pub fn restore_all(smc: &mut dyn SmcIo) {
    for i in 0..fan_count(smc) {
        let _ = set_auto(smc, i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A simulated Mac fan controller that behaves like real hardware: macOS's thermal
    /// service ignores manual-mode writes for a while (or until "Ftst" is set), and
    /// writing a key that doesn't exist fails.
    struct FakeMac {
        name: &'static str,
        keys: HashMap<String, f64>,
        /// mode writes are rejected while Ftst exists and isn't 1 (M3/M4 behaviour)
        needs_ftst: bool,
        /// mode writes needed before macOS lets a fan go manual
        writes_to_yield: u32,
        mode_writes: HashMap<String, u32>,
    }

    impl FakeMac {
        fn new(name: &'static str, keys: &[(&str, f64)], needs_ftst: bool, writes_to_yield: u32) -> Self {
            FakeMac {
                name,
                keys: keys.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                needs_ftst,
                writes_to_yield,
                mode_writes: HashMap::new(),
            }
        }
        /// measured on the user's M1 Max (macOS 27.2): no Ftst, F0Md/F1Md, ~3 s of retries
        fn m1_max_macos27() -> Self {
            Self::new("M1 Max, macOS 27 (no Ftst)", &[("FNum", 2.0), ("F0Md", 0.0), ("F1Md", 0.0), ("F0Tg", 1500.0), ("F1Tg", 1500.0)], false, 30)
        }
        /// M3/M4 as described by Stats, fan-dial and others: mode writes fail until Ftst = 1
        fn m3_with_ftst() -> Self {
            Self::new("M3/M4 (Ftst unlock)", &[("FNum", 2.0), ("Ftst", 0.0), ("F0Md", 0.0), ("F1Md", 0.0), ("F0Tg", 1500.0), ("F1Tg", 1500.0)], true, 20)
        }
        /// M5 Pro report (ThermalForge #23): lowercase F0md/F1md, no Ftst, direct write works
        fn m5() -> Self {
            Self::new("M5 (lowercase F0md)", &[("FNum", 2.0), ("F0md", 0.0), ("F1md", 0.0), ("F0Tg", 1350.0), ("F1Tg", 1350.0)], false, 1)
        }
        /// Intel: one FS! bitmask for all fans
        fn intel() -> Self {
            Self::new("Intel (FS! mask)", &[("FNum", 2.0), ("FS! ", 0.0), ("F0Tg", 2000.0), ("F1Tg", 2000.0)], false, 1)
        }
        fn get(&self, k: &str) -> f64 {
            self.keys[k]
        }
    }

    impl SmcIo for FakeMac {
        fn has_key(&mut self, key: &str) -> bool {
            self.keys.contains_key(key)
        }
        fn read_f64(&mut self, key: &str) -> Option<f64> {
            self.keys.get(key).copied()
        }
        fn write_f64(&mut self, key: &str, v: f64) -> Res<()> {
            if !self.keys.contains_key(key) {
                return Err(format!("{key}: no such key"));
            }
            let is_mode = key.len() == 4 && key.starts_with('F') && (key.ends_with("Md") || key.ends_with("md"));
            if is_mode && v == 1.0 {
                if self.needs_ftst && self.keys.get("Ftst") != Some(&1.0) {
                    return Err(format!("{key}: write rejected (0x82)"));
                }
                let n = self.mode_writes.entry(key.to_string()).or_insert(0);
                *n += 1;
                if *n < self.writes_to_yield {
                    return Ok(()); // accepted, but macOS still holds the fan
                }
            }
            self.keys.insert(key.to_string(), v);
            Ok(())
        }
        fn write_bytes(&mut self, key: &str, bytes: &[u8]) -> Res<()> {
            if !self.keys.contains_key(key) || bytes.len() != 2 {
                return Err(format!("{key}: bad write"));
            }
            self.keys.insert(key.to_string(), u16::from_be_bytes([bytes[0], bytes[1]]) as f64);
            Ok(())
        }
        fn read_bytes(&mut self, _key: &str) -> Option<Vec<u8>> {
            None
        }
    }

    fn all_macs() -> Vec<FakeMac> {
        vec![FakeMac::m1_max_macos27(), FakeMac::m3_with_ftst(), FakeMac::m5(), FakeMac::intel()]
    }

    #[test]
    fn takes_control_of_every_fan_on_every_kind_of_mac() {
        for mut mac in all_macs() {
            for i in 0..2 {
                set_manual(&mut mac, i, 3000.0 + i as f64)
                    .unwrap_or_else(|e| panic!("{}: fan {i}: {e}", mac.name));
                assert!(is_manual(&mut mac, i), "{}: fan {i} not manual", mac.name);
                assert_eq!(mac.get(&format!("F{i}Tg")), 3000.0 + i as f64, "{}: target", mac.name);
            }
        }
    }

    #[test]
    fn hands_every_fan_back_and_clears_the_unlock() {
        for mut mac in all_macs() {
            for i in 0..2 {
                set_manual(&mut mac, i, 3000.0).unwrap();
            }
            for i in 0..2 {
                set_auto(&mut mac, i).unwrap();
                assert!(!is_manual(&mut mac, i), "{}: fan {i} still manual", mac.name);
            }
            if mac.keys.contains_key("Ftst") {
                assert_eq!(mac.get("Ftst"), 0.0, "{}: Ftst left set", mac.name);
            }
        }
    }

    #[test]
    fn startup_frees_fans_left_by_a_killed_helper() {
        // a helper was SIGKILLed while holding both fans (and Ftst on M3/M4)
        for mut mac in all_macs() {
            for i in 0..2 {
                set_manual(&mut mac, i, 2500.0).unwrap();
            }
            assert_eq!(release_stale(&mut mac), 2, "{}", mac.name);
            for i in 0..2 {
                assert!(!is_manual(&mut mac, i), "{}: fan {i} still held", mac.name);
            }
            if mac.keys.contains_key("Ftst") {
                assert_eq!(mac.get("Ftst"), 0.0, "{}", mac.name);
            }
        }
    }

    #[test]
    fn reports_failure_when_macos_never_lets_go() {
        let mut mac = FakeMac::m1_max_macos27();
        mac.writes_to_yield = u32::MAX;
        let e = set_manual(&mut mac, 0, 3000.0).unwrap_err();
        assert!(e.contains("didn't hand over"), "{e}");
        // a fan we never got must not be reported as manual
        assert!(!is_manual(&mut mac, 0));
    }

    #[test]
    fn refuses_invalid_speeds_before_touching_the_hardware() {
        let mut mac = FakeMac::m1_max_macos27();
        for bad in [f64::NAN, f64::INFINITY, -1.0] {
            assert!(set_manual(&mut mac, 0, bad).is_err());
        }
        assert_eq!(mac.get("F0Md"), 0.0);
        assert_eq!(mac.get("F0Tg"), 1500.0);
    }
    /// Read-only: which fan-control keys this Mac has (cargo test -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn fan_keys_on_this_mac() {
        let mut smc = crate::smc::Smc::open().unwrap();
        for k in ["Ftst", "F0Md", "F0md", "F1Md", "F1md", "F0Tg", "FS! "] {
            let info = smc.has_key(k);
            let v = smc.read_f64(k);
            println!("{k:5} present={info} value={v:?}");
        }
    }
}
