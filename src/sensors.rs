//! Temperature sensor discovery via SMC "T***" keys.
//!
//! Apple reuses SMC keys with different meanings across chip generations (Tp09 is an
//! efficiency core on M1 but a performance core on M2), so core names come from a table
//! per generation. The tables follow the ones maintained by the open-source Stats app
//! (github.com/exelban/stats, Modules/Sensors/values.swift). On a chip newer than these
//! tables, every CPU/GPU-family key (Tp/Te/Tf/Tg) still counts toward "Hottest CPU/GPU",
//! so Smart Cooling always has a chip temperature to work from.

use crate::smc::Smc;
use std::sync::OnceLock;

#[derive(Debug, Clone)]
pub struct Reading {
    pub key: String,
    pub name: String,
    pub value: f64,
    pub named: bool,
}

pub const CPU_AVG_KEY: &str = "avg:cpu";
pub const GPU_AVG_KEY: &str = "avg:gpu";
pub const HOT_KEY: &str = "max:hot";

/// Which chip this Mac has, from the CPU brand string ("Apple M1 Max", "Intel(R) Core…").
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chip {
    /// Apple silicon: generation (1 = M1 …) and whether it's a Pro/Max/Ultra part
    Apple { gen: u32, big: bool },
    /// Apple silicon we can't place (new naming, A-series…)
    AppleUnknown,
    Intel,
}

impl Chip {
    pub fn parse(brand: &str) -> Chip {
        if let Some(rest) = brand.strip_prefix("Apple M") {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(gen) = digits.parse::<u32>() {
                let big = ["Pro", "Max", "Ultra"].iter().any(|v| rest.contains(v));
                return Chip::Apple { gen, big };
            }
            return Chip::AppleUnknown;
        }
        if brand.starts_with("Apple") {
            return Chip::AppleUnknown;
        }
        Chip::Intel
    }
}

/// The chip in this Mac (read once).
pub fn chip() -> Chip {
    static CHIP: OnceLock<Chip> = OnceLock::new();
    *CHIP.get_or_init(|| Chip::parse(&crate::smc::sysctl_string("machdep.cpu.brand_string")))
}

fn cores(keys: &[&'static str], kind: &str) -> Vec<(&'static str, String)> {
    keys.iter().enumerate().map(|(i, k)| (*k, format!("{kind} {}", i + 1))).collect()
}

const EFF: &str = "CPU Efficiency Core";
const PERF: &str = "CPU Performance Core";
const SUPER: &str = "CPU Super Core";
const GPU: &str = "GPU Core";

/// CPU/GPU core sensors for one Apple silicon generation.
fn core_table(chip: Chip) -> Vec<(&'static str, String)> {
    let Chip::Apple { gen, big } = chip else {
        return Vec::new();
    };
    let mut t = Vec::new();
    match gen {
        1 => {
            t.extend(cores(&["Tp09", "Tp0T"], EFF));
            t.extend(cores(&["Tp01", "Tp05", "Tp0D", "Tp0H", "Tp0L", "Tp0P", "Tp0X", "Tp0b"], PERF));
            t.extend(cores(&["Tg05", "Tg0D", "Tg0L", "Tg0T"], GPU));
        }
        2 => {
            t.extend(cores(&["Tp1h", "Tp1t", "Tp1p", "Tp1l"], EFF));
            t.extend(cores(&["Tp01", "Tp05", "Tp09", "Tp0D", "Tp0X", "Tp0b", "Tp0f", "Tp0j"], PERF));
            t.extend(cores(&["Tg0f", "Tg0j"], GPU));
        }
        3 => {
            t.extend(cores(&["Te05", "Te0L", "Te0P", "Te0S"], EFF));
            t.extend(cores(
                &["Tf04", "Tf09", "Tf0A", "Tf0B", "Tf0D", "Tf0E", "Tf44", "Tf49", "Tf4A", "Tf4B", "Tf4D", "Tf4E"],
                PERF,
            ));
            t.extend(cores(&["Tf14", "Tf18", "Tf19", "Tf1A", "Tf24", "Tf28", "Tf29", "Tf2A"], GPU));
        }
        4 => {
            t.extend(cores(&["Te05", "Te0S", "Te09", "Te0H"], EFF));
            t.extend(cores(&["Tp01", "Tp05", "Tp09", "Tp0D", "Tp0V", "Tp0Y", "Tp0b", "Tp0e"], PERF));
            let first: &[&str] = if big { &["Tg1U", "Tg1k"] } else { &["Tg0G", "Tg0H"] };
            let mut g: Vec<&'static str> = first.to_vec();
            g.extend(["Tg0K", "Tg0L", "Tg0d", "Tg0e", "Tg0j", "Tg0k"]);
            t.extend(cores(&g, GPU));
        }
        5 => {
            t.extend(cores(&["Tp00", "Tp04", "Tp08", "Tp0C", "Tp0G", "Tp0K"], SUPER));
            t.extend(cores(
                &["Tp0O", "Tp0R", "Tp0U", "Tp0X", "Tp0a", "Tp0d", "Tp0g", "Tp0j", "Tp0m", "Tp0p", "Tp0u", "Tp0y"],
                PERF,
            ));
            t.extend(cores(&["Tg0U", "Tg0X", "Tg0d", "Tg0g", "Tg0j", "Tg1Y", "Tg1c", "Tg1g"], GPU));
        }
        6 => {
            t.extend(cores(&["Tp0j", "Tp0g", "Tp09", "Tp07"], SUPER));
            t.extend(cores(&["Tp0m", "Tp0L", "Tp0I", "Tp0d", "Tp0E", "Tp0G", "Tp0b", "Tp05"], PERF));
            t.extend(cores(&["Te07", "Te08", "Te09", "Te0c", "Te0h", "Te0i"], EFF));
            t.extend(cores(
                &["Tg1e", "Tg0d", "Tg0c", "Tg0b", "Tg1c", "Tg1d", "Tg07", "Tg1f", "Tg09", "Tg0a", "Tg1g", "Tg1h"],
                GPU,
            ));
        }
        _ => {}
    }
    t
}

/// Sensors that mean the same thing on every Mac that has them.
fn common_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "TB0T" => "Battery",
        "TB1T" => "Battery Gas Gauge",
        "TB2T" => "Battery Sensor 2",
        "TW0P" => "Airport Proximity",
        "Ts0P" => "Palm Rest",
        "Ts1P" => "Palm Rest 2",
        "TaLP" => "Airflow Left",
        "TaRF" => "Airflow Right",
        "TH0x" => "SSD",
        "TH0a" => "SSD 1",
        "TH0b" => "SSD 2",
        "Tm0P" => "Mainboard",
        "TPCD" => "Platform Controller Hub",
        "TC0P" => "CPU Proximity",
        "TC0D" | "TC0E" => "CPU Die",
        "TCXC" => "CPU PECI",
        "TG0P" => "GPU Proximity",
        "TG0D" => "GPU Die",
        "Ts0S" => "Memory Proximity",
        "TA0P" => "Ambient",
        _ => return None,
    })
}

/// Apple silicon CPU/GPU sensor families (per-core keys start with these).
fn is_chip_family(key: &str) -> bool {
    ["Tp", "Te", "Tf", "Tg"].iter().any(|p| key.starts_with(p))
}

fn name_for(key: &str, chip: Chip, table: &[(&'static str, String)]) -> Option<String> {
    if let Some((_, n)) = table.iter().find(|(k, _)| *k == key) {
        return Some(n.clone());
    }
    if let Some(n) = common_name(key) {
        return Some(n.to_string());
    }
    let b = key.as_bytes();
    match chip {
        // Intel per-core sensors: TC1C, TC2C, …
        Chip::Intel if b.len() == 4 && b[0] == b'T' && b[1] == b'C' && b[3] == b'C' && b[2].is_ascii_digit() => {
            Some(format!("CPU Core {}", b[2] as char))
        }
        // a chip newer than our tables: still show (and use) its CPU/GPU sensors
        Chip::AppleUnknown | Chip::Apple { .. } if table.is_empty() && is_chip_family(key) => {
            Some(format!("Chip Sensor {key}"))
        }
        _ => None,
    }
}

fn plausible(v: f64) -> bool {
    v > 5.0 && v < 130.0
}

/// Enumerate all SMC keys once and keep the temperature ones that report a sane value.
pub fn discover(smc: &mut Smc) -> Vec<String> {
    let mut keys = Vec::new();
    for i in 0..smc.key_count() {
        let Ok(k) = smc.key_at(i) else { continue };
        if !k.starts_with('T') {
            continue;
        }
        if smc.read_f64(&k).is_some_and(plausible) {
            keys.push(k);
        }
    }
    keys
}

pub fn read_all(smc: &mut Smc, keys: &[String]) -> Vec<Reading> {
    let raw: Vec<(String, f64)> = keys
        .iter()
        .filter_map(|k| smc.read_f64(k).filter(|v| plausible(*v)).map(|v| (k.clone(), v)))
        .collect();
    summarize(&raw, chip())
}

/// Name the raw readings for this chip and add the averages and "Hottest CPU/GPU".
pub fn summarize(raw: &[(String, f64)], chip: Chip) -> Vec<Reading> {
    let table = core_table(chip);
    let mut out: Vec<Reading> = raw
        .iter()
        .map(|(k, v)| {
            let name = name_for(k, chip, &table);
            Reading {
                key: k.clone(),
                named: name.is_some(),
                name: name.unwrap_or_else(|| format!("SMC {k}")),
                value: *v,
            }
        })
        .collect();

    let avg = |prefixes: &[&str], out: &[Reading]| {
        let v: Vec<f64> = out
            .iter()
            .filter(|r| r.named && prefixes.iter().any(|p| r.name.starts_with(p)))
            .map(|r| r.value)
            .collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    };
    if let Some(v) = avg(&[EFF, PERF, SUPER, "CPU Core "], &out) {
        out.push(Reading { key: CPU_AVG_KEY.into(), name: "CPU Core Average".into(), value: v, named: true });
    }
    if let Some(v) = avg(&[GPU], &out) {
        out.push(Reading { key: GPU_AVG_KEY.into(), name: "GPU Core Average".into(), value: v, named: true });
    }
    // Hottest chip temperature. On Apple silicon: every CPU/GPU-family key (Tp/Te/Tf/Tg),
    // labelled or not. The labelled cores are only a subset: on an M1 Max the unlabelled
    // Tp0x/Te0x keys read ~10 degC hotter and match the SMC's own "CPU die max" (TCMz).
    // Sources also disagree on which keys are cores (Stats vs iSMC, especially on M3), so
    // the family maximum is the reading that doesn't depend on those labels.
    let named_max = out
        .iter()
        .filter(|r| r.named && (r.name.starts_with("CPU") || r.name.starts_with("GPU") || r.name.starts_with("Chip Sensor")))
        .map(|r| r.value)
        .fold(f64::NAN, f64::max);
    let hottest = if chip == Chip::Intel {
        named_max
    } else {
        out.iter().filter(|r| is_chip_family(&r.key)).map(|r| r.value).fold(named_max, f64::max)
    };
    if hottest.is_finite() {
        out.push(Reading { key: HOT_KEY.into(), name: "Hottest CPU/GPU".into(), value: hottest, named: true });
    }
    out.sort_by(|a, b| b.named.cmp(&a.named).then_with(|| a.name.cmp(&b.name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(keys: &[(&str, f64)]) -> Vec<(String, f64)> {
        keys.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }
    fn get<'a>(r: &'a [Reading], key: &str) -> Option<&'a Reading> {
        r.iter().find(|x| x.key == key)
    }

    #[test]
    fn chip_names_parse() {
        assert_eq!(Chip::parse("Apple M1 Max"), Chip::Apple { gen: 1, big: true });
        assert_eq!(Chip::parse("Apple M4"), Chip::Apple { gen: 4, big: false });
        assert_eq!(Chip::parse("Apple M5 Pro"), Chip::Apple { gen: 5, big: true });
        assert_eq!(Chip::parse("Apple M12 Ultra"), Chip::Apple { gen: 12, big: true });
        assert_eq!(Chip::parse("Apple A18 Pro"), Chip::AppleUnknown);
        assert_eq!(Chip::parse("Intel(R) Core(TM) i9-9880H CPU @ 2.30GHz"), Chip::Intel);
    }

    #[test]
    fn same_key_means_different_cores_per_generation() {
        let m1 = summarize(&raw(&[("Tp09", 60.0)]), Chip::Apple { gen: 1, big: true });
        let m2 = summarize(&raw(&[("Tp09", 60.0)]), Chip::Apple { gen: 2, big: false });
        assert_eq!(get(&m1, "Tp09").unwrap().name, "CPU Efficiency Core 1");
        assert_eq!(get(&m2, "Tp09").unwrap().name, "CPU Performance Core 3");
    }

    #[test]
    fn every_generation_gets_a_chip_temperature() {
        // a few real core keys per generation (from Stats' tables) plus non-core sensors
        let cases: &[(Chip, &[(&str, f64)])] = &[
            (Chip::Apple { gen: 1, big: true }, &[("Tp01", 71.0), ("Tg05", 66.0), ("TB0T", 33.0)]),
            (Chip::Apple { gen: 2, big: false }, &[("Tp1h", 55.0), ("Tp0f", 72.0), ("Tg0f", 60.0)]),
            (Chip::Apple { gen: 3, big: true }, &[("Te05", 50.0), ("Tf04", 74.0), ("Tf14", 65.0)]),
            (Chip::Apple { gen: 4, big: true }, &[("Te05", 51.0), ("Tp0V", 76.0), ("Tg1U", 62.0)]),
            (Chip::Apple { gen: 4, big: false }, &[("Te05", 51.0), ("Tp0e", 73.0), ("Tg0G", 61.0)]),
            (Chip::Apple { gen: 5, big: true }, &[("Tp00", 80.0), ("Tp0O", 75.0), ("Tg0U", 64.0)]),
            (Chip::Apple { gen: 6, big: false }, &[("Tp0j", 78.0), ("Te07", 52.0), ("Tg1e", 63.0)]),
        ];
        for (chip, keys) in cases {
            let r = summarize(&raw(keys), *chip);
            let hot = get(&r, HOT_KEY).unwrap_or_else(|| panic!("{chip:?}: no chip temperature"));
            let want = keys.iter().filter(|(k, _)| is_chip_family(k)).map(|(_, v)| *v).fold(f64::NAN, f64::max);
            assert_eq!(hot.value, want, "{chip:?}");
            assert!(get(&r, CPU_AVG_KEY).is_some(), "{chip:?}: no CPU average");
            assert!(get(&r, GPU_AVG_KEY).is_some(), "{chip:?}: no GPU average");
        }
    }

    #[test]
    fn unlabelled_chip_sensors_count_toward_the_hottest() {
        // measured on an M1 Max: labelled cores max 55.9, unlabelled Te02 65.9 (= TCMz die max)
        let r = summarize(&raw(&[("Tp01", 55.9), ("Te02", 65.9), ("Tp02", 65.4), ("TB0T", 33.0)]), Chip::Apple { gen: 1, big: true });
        assert_eq!(get(&r, HOT_KEY).unwrap().value, 65.9);
        // non-chip sensors never count
        let r = summarize(&raw(&[("Tp01", 55.0), ("TB0T", 90.0), ("TW0P", 95.0)]), Chip::Apple { gen: 1, big: true });
        assert_eq!(get(&r, HOT_KEY).unwrap().value, 55.0);
    }

    #[test]
    fn future_chips_still_get_a_chip_temperature() {
        // an M9 with keys nobody has mapped yet
        let r = summarize(&raw(&[("Tp7Q", 81.0), ("Tg3Z", 70.0), ("TB0T", 34.0)]), Chip::Apple { gen: 9, big: true });
        assert_eq!(get(&r, HOT_KEY).unwrap().value, 81.0);
        assert_eq!(get(&r, "Tp7Q").unwrap().name, "Chip Sensor Tp7Q");
        let r = summarize(&raw(&[("Tp7Q", 81.0)]), Chip::AppleUnknown);
        assert_eq!(get(&r, HOT_KEY).unwrap().value, 81.0);
    }

    #[test]
    fn intel_macs_use_core_and_die_sensors() {
        let r = summarize(&raw(&[("TC1C", 70.0), ("TC2C", 74.0), ("TC0P", 60.0), ("TB0T", 30.0)]), Chip::Intel);
        assert_eq!(get(&r, HOT_KEY).unwrap().value, 74.0);
        assert_eq!(get(&r, "TC2C").unwrap().name, "CPU Core 2");
    }
}
