//! Background thread that owns the SMC connection, polls sensors and applies fan modes.

use crate::fans::{self, FanInfo};
use crate::helper;
use crate::load::{self, CpuSampler};
use crate::sensors::{self, Reading};
use crate::smc::{is_root, Smc};
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum FanMode {
    Auto,
    Constant(f64),
    /// Linear ramp from fan min at lo degC to fan max at hi degC.
    Sensor {
        key: String,
        lo: f64,
        hi: f64,
    },
    /// Load- and trend-aware: fans stay with macOS during light work, and ramp
    /// up for sustained CPU/GPU load or when the temperature is heading past lo.
    Smart {
        lo: f64,
        hi: f64,
    },
}

impl FanMode {
    pub fn encode(&self) -> String {
        match self {
            FanMode::Auto => "auto".into(),
            FanMode::Constant(r) => format!("const,{r}"),
            FanMode::Sensor { key, lo, hi } => format!("sensor,{key},{lo},{hi}"),
            FanMode::Smart { lo, hi } => format!("smart,{lo},{hi}"),
        }
    }
    /// Parses a mode received from the app. Everything is validated here: the helper runs
    /// as root and these values end up in the fan controller.
    pub fn decode(s: &str) -> Option<FanMode> {
        let num = |v: &str| v.parse::<f64>().ok().filter(|x| x.is_finite());
        let p: Vec<&str> = s.split(',').collect();
        match p.as_slice() {
            ["auto"] => Some(FanMode::Auto),
            ["const", r] => Some(FanMode::Constant(num(r)?.clamp(0.0, 20_000.0))),
            ["smart", lo, hi] => {
                let (lo, hi) = crate::settings::clamp_smart(num(lo)?, num(hi)?);
                Some(FanMode::Smart { lo, hi })
            }
            ["sensor", k, lo, hi] => {
                if k.is_empty() || k.len() > 16 || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == ':') {
                    return None;
                }
                let lo = num(lo)?.clamp(20.0, 110.0);
                let hi = num(hi)?.clamp(lo + 1.0, 120.0);
                Some(FanMode::Sensor { key: k.to_string(), lo, hi })
            }
            _ => None,
        }
    }
}

pub enum Cmd {
    SetMode(usize, FanMode),
    Shutdown,
}

/// What the machine is doing, used by Smart Cooling and shown in the UI.
#[derive(Clone, Default, Debug)]
pub struct Activity {
    pub cpu: f64,
    pub gpu: Option<f64>,
    /// max(cpu, gpu) averaged over ~30 s
    pub sustained: f64,
    /// palm-rest temperature
    pub chassis: Option<f64>,
    /// air inside the back of the case
    pub body: Option<f64>,
    /// hottest CPU/GPU temperature trend, degC per minute
    pub trend: f64,
    /// Smart Cooling state, e.g. "Quiet" / "Cooling"
    pub smart: Option<String>,
    /// hottest battery sensor
    pub battery: Option<f64>,
    pub charging: bool,
}

/// One reading per second fed to Smart Cooling.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    /// hottest CPU/GPU core, degC
    pub chip: f64,
    /// palm-rest (chassis surface) temperature, degC, if this Mac has the sensor
    pub chassis: Option<f64>,
    /// air inside the back of the case (the airflow sensors by the vents), degC
    pub body: Option<f64>,
    /// max(CPU, GPU) utilisation right now, 0..1
    pub load: f64,
    /// hottest battery sensor, degC (None on Macs without a battery)
    pub battery: Option<f64>,
    /// the battery is charging (charging warms the battery even when the Mac is idle)
    pub charging: bool,
}

// Time constants (samples are 1 s apart).
const LOAD_TAU: f64 = 30.0; // load is averaged over ~30 s, so launches/bursts don't count
const CHIP_TAU: f64 = 10.0; // fast chip average: smooths per-core spikes, used for the trend
const CHIP_SLOW_TAU: f64 = 30.0; // slow chip average: what decisions are made on
const BODY_TAU: f64 = 15.0;
const MEDIAN_SECS: usize = 9; // rejects spikes shorter than ~4 s (app launches, single hot cores)
const TREND_SECS: usize = 60; // trend is fitted over a full minute of smoothed readings
// Decision thresholds.
const BUSY_LOAD: f64 = 0.50; // sustained load that counts as "busy"
// Air inside the back of the case. Measured on an M1 Pro: ~33 degC in light use,
// ~57 degC after a few minutes of Blender with macOS keeping the fans at minimum.
const BODY_WARM: f64 = 45.0; // busy + this: cool
const BODY_HOT: f64 = 52.0; // cool whatever the load
const BODY_ZERO: f64 = 42.0; // fan curve: minimum speed here ...
const BODY_MAX: f64 = 64.0; // ... full speed here
const CHASSIS_WARM: f64 = 35.0; // palm rest: warm enough to notice
const CHASSIS_HOT: f64 = 38.0; // palm rest: cool even without load
const CHASSIS_MAX: f64 = 42.0; // palm rest: full speed
const START_SECS: u32 = 20; // case/palm-rest heat must hold this long before fans start
const CHIP_START_SECS: u32 = 150; // busy + hot chip alone must hold this long
const HOT_START_SECS: u32 = 45; // chip at "full speed" temperature, whatever the load
const STOP_SECS: u32 = 45; // calm must hold this long before fans hand back to macOS
// Fan speed moves in fixed steps so it holds steady instead of hunting.
const LEVELS: f64 = 8.0;
const STEP_DOWN_SECS: u32 = 30; // headroom must last this long before stepping down
// Heat soak: the curve alone settles wherever heat in = heat out, which under hours of
// Blender left the case air at ~51 degC and the bottom case hot at ~3000 rpm. While the case
// air (or palm rest) stays above its target the fans keep stepping up until it comes down.
// Battery. Charging heats it even with the Mac idle (measured: 42 degC while charging at
// low load), and sustained heat above ~40 degC wears lithium cells faster.
const BATTERY_WARM: f64 = 38.0; // charging + this: cool
const BATTERY_HOT: f64 = 41.0; // cool whatever is going on
const BATTERY_ZERO: f64 = 37.0; // fan curve: minimum speed here ...
const BATTERY_MAX: f64 = 47.0; // ... full speed here
const BATTERY_TARGET: f64 = 39.0; // battery temperature the fans work toward while cooling
const BODY_TARGET: f64 = 45.0; // case air the fans work toward while cooling
const CHASSIS_TARGET: f64 = 36.0; // palm rest the fans work toward while cooling
const SOAK_RATE: f64 = 0.0005; // per degC over target per second: 5 degC over = 1 step / 50 s
const SOAK_RELEASE: f64 = 0.001; // per second once 1 degC under target: 1 step / ~2 min

/// Smart Cooling. Fans take over from macOS when the heat is real and sustained:
/// - the air in the back of the case is warming (busy + >=45 degC, or >=52 degC), or
/// - the palm rest is warm (busy + >=35 degC, or >=38 degC), or
/// - the chip has stayed hot (past halfway to "full speed") through minutes of heavy work,
/// - or, as a safety net, the chip is held at the "full speed" temperature.
/// Chip readings are spike-filtered and averaged over ~30 s, so app launches never count.
/// Speed follows the case air most of all; the chip counts at half weight (Apple Silicon
/// runs in the 80s by design) until it reaches "full speed". The output moves in eight
/// fixed steps: up promptly, down only after 30 s of headroom.
#[derive(Default)]
pub struct SmartCtl {
    load_ema: Option<f64>,
    chip_ema: Option<f64>,
    chip_slow: Option<f64>,
    body_ema: Option<f64>,
    chip_raw: VecDeque<f64>,
    chip_hist: VecDeque<f64>,
    active: bool,
    want_for: u32,
    warm_for: u32,
    hot_for: u32,
    calm_for: u32,
    level: f64,
    down_for: u32,
    /// extra fan fraction built up while the heat stays above target
    soak: f64,
}

fn ema(prev: Option<f64>, x: f64, tau: f64) -> f64 {
    match prev {
        Some(p) => p + (x - p) / tau,
        None => x,
    }
}

impl SmartCtl {
    pub fn load(&self) -> f64 {
        self.load_ema.unwrap_or(0.0)
    }

    /// degC per minute over the last minute of smoothed readings (0 until a full minute)
    pub fn trend(&self) -> f64 {
        if self.chip_hist.len() < TREND_SECS {
            return 0.0;
        }
        let v: Vec<f64> = self.chip_hist.iter().copied().collect();
        slope(&v) * 60.0
    }

    /// Returns Some(fraction 0..1 of the fan range) while cooling, None while macOS should keep the fans.
    pub fn step(&mut self, s: &Sample, lo: f64, hi: f64) -> (Option<f64>, String) {
        let load = ema(self.load_ema, s.load, LOAD_TAU);
        self.chip_raw.push_back(s.chip);
        while self.chip_raw.len() > MEDIAN_SECS {
            self.chip_raw.pop_front();
        }
        let mut sorted: Vec<f64> = self.chip_raw.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let med = sorted[sorted.len() / 2];
        let fast = ema(self.chip_ema, med, CHIP_TAU);
        let chip = ema(self.chip_slow, med, CHIP_SLOW_TAU);
        self.load_ema = Some(load);
        self.chip_ema = Some(fast);
        self.chip_slow = Some(chip);
        let body = s.body.map(|b| ema(self.body_ema, b, BODY_TAU));
        self.body_ema = body.or(self.body_ema);
        self.chip_hist.push_back(fast);
        while self.chip_hist.len() > TREND_SECS {
            self.chip_hist.pop_front();
        }
        let trend = self.trend();
        let busy = load >= BUSY_LOAD;

        let body_warm = body.is_some_and(|b| b >= BODY_WARM);
        let body_hot = body.is_some_and(|b| b >= BODY_HOT);
        let chassis_warm = s.chassis.is_some_and(|c| c >= CHASSIS_WARM);
        let chassis_hot = s.chassis.is_some_and(|c| c >= CHASSIS_HOT);
        let battery_warm = s.charging && s.battery.is_some_and(|b| b >= BATTERY_WARM);
        let battery_hot = s.battery.is_some_and(|b| b >= BATTERY_HOT);
        // halfway between "start" and "full speed" (74 degC by default)
        let chip_warm = busy && chip >= lo + (hi - lo) * 0.5;
        let chip_hot = chip >= hi;
        let tick = |n: u32, c: bool| if c { n + 1 } else { 0 };

        if !self.active {
            self.want_for = tick(
                self.want_for,
                (busy && (body_warm || chassis_warm)) || body_hot || chassis_hot || battery_warm || battery_hot,
            );
            self.warm_for = tick(self.warm_for, chip_warm);
            self.hot_for = tick(self.hot_for, chip_hot);
            if self.want_for >= START_SECS
                || self.warm_for >= CHIP_START_SECS
                || self.hot_for >= HOT_START_SECS
            {
                self.active = true;
                self.calm_for = 0;
                self.level = -1.0; // set from the first target below
                self.down_for = 0;
                self.soak = 0.0;
            }
        } else {
            // stop when the heat is gone, whatever the load (hysteresis below the start points)
            let chassis_cool = s.chassis.map_or(true, |c| c < CHASSIS_WARM - 1.0);
            let body_cool = body.map_or(true, |b| b < BODY_WARM - 3.0);
            let battery_cool = s.battery.map_or(true, |b| b < BATTERY_WARM - 1.0);
            let calm = chassis_cool
                && body_cool
                && battery_cool
                && chip < hi - 3.0
                && (chip < lo || (!busy && trend < 0.5));
            self.calm_for = if calm { self.calm_for + 1 } else { 0 };
            if self.calm_for >= STOP_SECS {
                self.active = false;
                self.want_for = 0;
                self.warm_for = 0;
                self.hot_for = 0;
            }
        }

        if !self.active {
            let why = if self.want_for + self.warm_for + self.hot_for > 0 {
                "Quiet: watching, heat is building"
            } else {
                "Quiet: no sustained heat, fans left to macOS"
            };
            return (None, why.into());
        }
        let span = (hi - lo).max(1.0);
        let by_chip = if chip < hi {
            0.5 * ((chip + trend.max(0.0) * 0.5 - lo) / span).clamp(0.0, 1.0)
        } else {
            0.5 + 0.5 * ((chip - hi) / 6.0).clamp(0.0, 1.0)
        };
        let by_body = body.map_or(0.0, |b| ((b - BODY_ZERO) / (BODY_MAX - BODY_ZERO)).clamp(0.0, 1.0));
        let by_chassis = s.chassis.map_or(0.0, |c| {
            ((c - CHASSIS_WARM) / (CHASSIS_MAX - CHASSIS_WARM)).clamp(0.0, 1.0)
        });
        let by_battery = s.battery.map_or(0.0, |b| {
            ((b - BATTERY_ZERO) / (BATTERY_MAX - BATTERY_ZERO)).clamp(0.0, 1.0)
        });
        // build up while the heat stays above target, release slowly once it is below
        let over = body
            .map_or(f64::MIN, |b| b - BODY_TARGET)
            .max(s.chassis.map_or(f64::MIN, |c| c - CHASSIS_TARGET))
            .max(s.battery.map_or(f64::MIN, |b| b - BATTERY_TARGET));
        if over > 0.0 {
            self.soak += over * SOAK_RATE;
        } else if over < -1.0 {
            self.soak -= SOAK_RELEASE;
        }
        let base = by_chip.max(by_body).max(by_chassis).max(by_battery);
        self.soak = self.soak.clamp(0.0, 1.0 - base.min(1.0));
        let f = base + self.soak;

        // fixed steps with hysteresis: up as soon as needed, down after sustained headroom
        let want = (f * LEVELS).round() / LEVELS;
        let step = 1.0 / LEVELS;
        if self.level < 0.0 || f > self.level + step * 0.75 {
            self.level = want.max(self.level);
            self.down_for = 0;
        } else if f < self.level - step * 0.6 {
            self.down_for += 1;
            if self.down_for >= STEP_DOWN_SECS {
                self.level = (self.level - step).max(want);
                self.down_for = 0;
            }
        } else {
            self.down_for = 0;
        }

        let why = if self.calm_for > 0 {
            "Cooling: winding down"
        } else if self.soak >= 1.0 / LEVELS && over > 0.0 {
            "Cooling: heat is building up, pushing harder"
        } else if by_battery > 0.0 && by_battery >= by_body.max(by_chassis).max(by_chip) {
            if s.charging { "Cooling: battery is warm while charging" } else { "Cooling: battery is warm" }
        } else if by_body >= by_chip && by_body >= by_chassis && by_body > 0.0 {
            "Cooling: back of the case is warm"
        } else if by_chassis >= by_chip && by_chassis > 0.0 {
            "Cooling: palm rest is warm"
        } else if chip_hot {
            "Cooling: chip is hot"
        } else {
            "Cooling: sustained heavy load"
        };
        (Some(self.level.clamp(0.0, 1.0)), why.into())
    }
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub activity: Activity,
    pub ready: bool,
    pub root: bool,
    /// true when fans can be changed: running as root, or the helper is running
    pub can_control: bool,
    pub fans: Vec<FanInfo>,
    pub modes: Vec<FanMode>,
    pub sensors: Vec<Reading>,
    pub error: Option<String>,
}

pub struct Worker {
    pub snapshot: Arc<Mutex<Snapshot>>,
    tx: Sender<Cmd>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(on_update: impl Fn() + Send + 'static) -> Self {
        let snapshot = Arc::new(Mutex::new(Snapshot {
            root: is_root(),
            ..Default::default()
        }));
        let (tx, rx) = channel();
        let snap = snapshot.clone();
        let handle = std::thread::spawn(move || {
            let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(snap.clone(), rx, on_update)
            }))
            .is_err();
            if crashed {
                // Never leave fans held by a dead control loop: hand them back to macOS.
                crate::fans::restore_if_touched();
                if is_root() {
                    // The helper is useless without this thread. Exit (the exit handler
                    // also restores fans) so launchd starts a fresh, working helper.
                    std::process::exit(70);
                }
                snap.lock().unwrap_or_else(|e| e.into_inner()).error =
                    Some("Fan monitoring stopped unexpectedly. Restart Mac Smart Cooler.".into());
            }
        });
        Worker {
            snapshot,
            tx,
            handle: Some(handle),
        }
    }

    /// Hand a command to the control loop. false = the loop is gone (nothing will apply it).
    pub fn send(&self, cmd: Cmd) -> bool {
        self.tx.send(cmd).is_ok()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

pub const NEED_ROOT: &str = "Fan control isn't enabled yet: click “Enable fan control” at the top";

/// Air inside the back of the case: the hotter of the two airflow sensors by the vents.
pub fn case_air(readings: &[Reading]) -> Option<f64> {
    readings
        .iter()
        .filter(|r| r.key == "TaLP" || r.key == "TaRF")
        .map(|r| r.value)
        .filter(|v| (5.0..=110.0).contains(v))
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))
}

/// Battery temperature: the hottest of the battery sensors.
pub fn battery_temp(readings: &[Reading]) -> Option<f64> {
    readings
        .iter()
        .filter(|r| matches!(r.key.as_str(), "TB0T" | "TB1T" | "TB2T"))
        .map(|r| r.value)
        .filter(|v| (5.0..=90.0).contains(v))
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))
}

/// Least-squares slope of the samples, per sample step.
fn slope(v: &[f64]) -> f64 {
    let n = v.len() as f64;
    if n < 3.0 {
        return 0.0;
    }
    let mx = (n - 1.0) / 2.0;
    let my = v.iter().sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (i, y) in v.iter().enumerate() {
        let dx = i as f64 - mx;
        num += dx * (y - my);
        den += dx * dx;
    }
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

fn desired_rpm(mode: &FanMode, fan: &FanInfo, sensors: &[Reading]) -> Option<f64> {
    // Unreadable limits (0/0) would make clamp() panic; never drive a fan we can't bound.
    if !(fan.max > fan.min) || !fan.min.is_finite() {
        return None;
    }
    match mode {
        FanMode::Auto | FanMode::Smart { .. } => None,
        FanMode::Constant(r) => Some(r.clamp(fan.min, fan.max)),
        FanMode::Sensor { key, lo, hi } => {
            let t = sensors.iter().find(|r| &r.key == key)?.value;
            let f = if hi <= lo {
                if t >= *hi {
                    1.0
                } else {
                    0.0
                }
            } else {
                ((t - lo) / (hi - lo)).clamp(0.0, 1.0)
            };
            Some(fan.min + f * (fan.max - fan.min))
        }
    }
}

fn run(snap: Arc<Mutex<Snapshot>>, rx: Receiver<Cmd>, on_update: impl Fn()) {
    let root = is_root();
    let mut smc = match Smc::open() {
        Ok(s) => s,
        Err(e) => {
            snap.lock().unwrap_or_else(|e| e.into_inner()).error = Some(e);
            on_update();
            return;
        }
    };
    let keys = sensors::discover(&mut smc);
    let n = fans::fan_count(&mut smc);
    let mut modes = vec![FanMode::Auto; n];
    let mut error: Option<String> = None;
    let mut smoothed: Vec<Option<f64>> = vec![None; n];
    let mut cpu = CpuSampler::new();
    let mut smart = SmartCtl::default();
    // after a failed write, leave that fan with macOS for a while, then retry (mode is kept)
    let mut retry_at: Vec<Option<std::time::Instant>> = vec![None; n];

    loop {
        let mut fan_list: Vec<FanInfo> = (0..n).map(|i| fans::read_fan(&mut smc, i, n)).collect();
        let readings = sensors::read_all(&mut smc, &keys);

        // ---- activity: utilisation + temperatures, tracked over time ----
        let mut act = Activity {
            cpu: cpu.sample().unwrap_or(0.0),
            gpu: load::gpu_utilization(),
            ..Default::default()
        };
        let hot = readings
            .iter()
            .find(|r| r.key == sensors::HOT_KEY)
            .map(|r| r.value);
        let chassis = readings
            .iter()
            .filter(|r| r.key == "Ts0P" || r.key == "Ts1P")
            .map(|r| r.value)
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
        let body = case_air(&readings);
        let battery = battery_temp(&readings);
        let charging = crate::load::charging().unwrap_or(false);
        let smart_params = modes.iter().find_map(|m| match m {
            FanMode::Smart { lo, hi } => Some((*lo, *hi)),
            _ => None,
        });
        let (lo, hi) = smart_params.unwrap_or((60.0, 88.0));
        let mut smart_f = None;
        if let Some(t) = hot {
            // always tracked, so switching to Smart Cooling starts with real history
            let (f, why) = smart.step(
                &Sample {
                    chip: t,
                    chassis,
                    body,
                    load: act.cpu.max(act.gpu.unwrap_or(0.0)),
                    battery,
                    charging,
                },
                lo,
                hi,
            );
            if smart_params.is_some() && root {
                act.smart = Some(why);
                smart_f = f;
            }
        }
        act.sustained = smart.load();
        act.trend = smart.trend();
        act.chassis = chassis;
        act.body = body;
        act.battery = battery;
        act.charging = charging;

        if root {
            for (i, fan) in fan_list.iter_mut().enumerate() {
                let is_smart = matches!(modes[i], FanMode::Smart { .. });
                let mut want = if is_smart {
                    smart_f
                        .filter(|_| fan.max > fan.min)
                        .map(|f| fan.min + f * (fan.max - fan.min))
                } else {
                    desired_rpm(&modes[i], fan, &readings)
                };
                if let (Some(w), FanMode::Sensor { .. } | FanMode::Smart { .. }) = (want, &modes[i])
                {
                    // speed up quickly (300 rpm/s), slow down gently (80 rpm/s): responsive but quiet
                    let s = match smoothed[i] {
                        Some(prev) if w > prev => (prev + 300.0).min(w),
                        Some(prev) => (prev - 80.0).max(w),
                        None => w,
                    };
                    smoothed[i] = Some(s);
                    want = Some(s);
                } else {
                    smoothed[i] = None;
                }
                if want.is_none() && is_smart && fan.manual {
                    // quiet again: give this fan back to macOS
                    if let Err(e) = fans::set_auto(&mut smc, i) {
                        error = Some(e);
                    }
                    fan.manual = false;
                    continue;
                }
                if retry_at[i].is_some_and(|t| std::time::Instant::now() < t) {
                    continue;
                }
                if let Some(rpm) = want {
                    if !fan.manual || (fan.target - rpm).abs() > 25.0 {
                        match fans::set_manual(&mut smc, i, rpm) {
                            Ok(()) => {
                                fan.manual = true;
                                fan.target = rpm;
                                retry_at[i] = None;
                            }
                            Err(e) => {
                                // don't retry (and block) every second: give the fan back to macOS
                                // for 30 s, then try again. The user's chosen mode is kept.
                                error = Some(format!("{e} (retrying in 30 s)"));
                                retry_at[i] =
                                    Some(std::time::Instant::now() + Duration::from_secs(30));
                                let _ = fans::set_auto(&mut smc, i);
                            }
                        }
                    }
                }
            }
        }

        // Not root: the helper daemon (if installed) owns the fans; mirror its state.
        let mut remote = false;
        let mut remote_modes = vec![FanMode::Auto; n];
        let mut remote_err = None;
        let mut remote_smart = None;
        if !root {
            if let Ok(reply) = helper::rpc("get") {
                remote = true;
                let mut parts = reply.split('\t');
                let m = parts.next().unwrap_or("");
                let e = parts.next().unwrap_or("");
                remote_smart = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
                for (i, enc) in m.split('|').enumerate().take(n) {
                    if let Some(mode) = FanMode::decode(enc) {
                        remote_modes[i] = mode;
                    }
                }
                if !e.is_empty() {
                    remote_err = Some(e.to_string());
                }
            }
        }

        {
            let mut s = snap.lock().unwrap_or_else(|e| e.into_inner());
            s.ready = true;
            s.fans = fan_list;
            s.can_control = root || remote;
            if remote {
                act.smart = remote_smart;
            }
            s.activity = act;
            s.modes = if root { modes.clone() } else { remote_modes };
            s.sensors = readings;
            s.error = error.clone().or(remote_err);
        }
        on_update();

        // Wait up to a second for a command, then take every queued one at once: a preset
        // sends one per fan, and handling them a loop apart made switching feel sluggish.
        let mut cmds = Vec::new();
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(c) => cmds.push(c),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => cmds.push(Cmd::Shutdown),
        }
        while let Ok(c) = rx.try_recv() {
            cmds.push(c);
        }
        let changed = cmds.iter().any(|c| matches!(c, Cmd::SetMode(..)));
        for cmd in cmds {
            match cmd {
                Cmd::SetMode(i, mode) if i < n => {
                    error = None;
                    if !root {
                        if remote {
                            match helper::rpc(&format!("set {i} {}", mode.encode())) {
                                Ok(r) if r == "ok" => {}
                                Ok(r) => error = Some(format!("helper: {r}")),
                                Err(e) => error = Some(e),
                            }
                        } else if mode != FanMode::Auto || fans::read_fan(&mut smc, i, n).manual {
                            // without root or helper we can't write anything
                            error = Some(NEED_ROOT.into());
                        }
                        continue;
                    }
                    if mode == FanMode::Auto {
                        if let Err(e) = fans::set_auto(&mut smc, i) {
                            error = Some(e);
                        }
                    }
                    modes[i] = mode;
                }
                Cmd::SetMode(..) => {}
                Cmd::Shutdown => {
                    if root && modes.iter().any(|m| *m != FanMode::Auto) {
                        fans::restore_all(&mut smc);
                    }
                    return;
                }
            }
        }
        if changed && root {
            // publish new modes now (not after the next sensor sweep) so the app sees them at once
            snap.lock().unwrap_or_else(|e| e.into_inner()).modes = modes.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate second by second; returns the first second the fans came on (if any)
    /// and whether they were on at the end.
    fn sim(ctl: &mut SmartCtl, secs: u32, f: impl Fn(u32) -> Sample) -> (Option<u32>, bool) {
        let mut first = None;
        let mut on = false;
        for t in 0..secs {
            on = ctl.step(&f(t), 60.0, 88.0).0.is_some();
            if on && first.is_none() {
                first = Some(t);
            }
        }
        (first, on)
    }
    fn idle(t: u32) -> Sample {
        let _ = t;
        Sample { battery: None, charging: false, body: None,
            chip: 48.0,
            chassis: Some(30.0),
            load: 0.05,
        }
    }

    #[test]
    fn launcher_or_app_start_stays_quiet() {
        // 30 s of 80% CPU while the chip warms 48 -> 62, then it settles back
        let mut c = SmartCtl::default();
        sim(&mut c, 60, idle);
        let (first, _) = sim(&mut c, 180, |t| {
            if t < 30 {
                Sample { battery: None, charging: false, body: None,
                    chip: 48.0 + 14.0 * t as f64 / 30.0,
                    chassis: Some(30.5),
                    load: 0.8,
                }
            } else {
                Sample { battery: None, charging: false, body: None,
                    chip: (62.0 - (t - 30) as f64 * 0.3).max(48.0),
                    chassis: Some(30.5),
                    load: 0.1,
                }
            }
        });
        assert_eq!(first, None, "a short burst must not spin the fans");
    }

    #[test]
    fn heavy_load_on_a_cool_chip_stays_quiet() {
        // load alone never starts the fans
        let mut c = SmartCtl::default();
        let (first, _) = sim(&mut c, 300, |_| Sample { battery: None, charging: false, body: None,
            chip: 50.0,
            chassis: Some(30.0),
            load: 0.9,
        });
        assert_eq!(first, None);
    }

    #[test]
    fn gaming_turns_fans_on_while_heating() {
        // sustained 85% load, chip climbs 50 -> 80 over 2 min, chassis slowly warms
        let mut c = SmartCtl::default();
        sim(&mut c, 60, idle);
        let (first, on) = sim(&mut c, 300, |t| Sample { battery: None, charging: false, body: None,
            chip: (50.0 + 0.25 * t as f64).min(80.0),
            chassis: Some(30.0 + 7.0 * t as f64 / 300.0),
            load: 0.85,
        });
        let first = first.expect("fans should start during sustained heavy, heating work");
        assert!((20..=300).contains(&first), "started at {first}s");
        assert!(on);
    }

    #[test]
    fn warm_chip_with_cool_chassis_stays_quiet() {
        // Apple Silicon cores sit in the 70s during ordinary work; that alone is not a reason
        let mut c = SmartCtl::default();
        let (first, _) = sim(&mut c, 300, |_| Sample { battery: None, charging: false, body: None,
            chip: 76.0,
            chassis: Some(31.0),
            load: 0.3,
        });
        assert_eq!(first, None);
    }

    #[test]
    fn very_hot_chip_without_load_still_cools() {
        let mut c = SmartCtl::default();
        let (first, _) = sim(&mut c, 120, |_| Sample { battery: None, charging: false, body: None,
            chip: 92.0,
            chassis: Some(31.0),
            load: 0.1,
        });
        assert!(first.is_some());
    }

    /// Deterministic jitter like the real hottest-core reading (+-4 degC second to second).
    fn jitter(t: u32) -> f64 {
        let x = (t.wrapping_mul(2654435761) >> 7) % 1000;
        (x as f64 / 1000.0 - 0.5) * 8.0
    }

    #[test]
    fn noisy_sensor_and_spikes_stay_quiet() {
        // measured live: chip 40-49 jumping every second, cpu 30-50%, gpu bursts; plus
        // occasional 1-3 s spikes to 80+ when an app launches
        let mut c = SmartCtl::default();
        let (first, _) = sim(&mut c, 900, |t| {
            let spike = if t % 97 < 3 { 35.0 } else { 0.0 };
            Sample { battery: None, charging: false, body: None,
                chip: 45.0 + jitter(t) + spike,
                chassis: Some(24.0),
                load: 0.35 + jitter(t + 7) / 20.0,
            }
        });
        assert_eq!(first, None);
        assert!(
            c.trend().abs() < 1.5,
            "trend {} should be ~0 on a flat noisy signal",
            c.trend()
        );
    }

    #[test]
    fn steady_gpu_load_at_70_with_cool_palm_rest_stays_quiet() {
        // measured: Unreal Editor open, GPU ~100% for minutes, hottest core ~70 degC, palm rest 30 degC
        let mut c = SmartCtl::default();
        let (first, _) = sim(&mut c, 1200, |t| Sample { battery: None, charging: false, body: None, chip: 70.0 + jitter(t) / 2.0, chassis: Some(30.0), load: 0.99 });
        assert_eq!(first, None);
    }

    #[test]
    fn steady_gpu_load_with_warming_palm_rest_cools() {
        let mut c = SmartCtl::default();
        let (first, _) = sim(&mut c, 1200, |t| Sample { battery: None, charging: false, body: None, chip: 70.0, chassis: Some(30.0 + t as f64 / 120.0), load: 0.99 });
        assert!(first.is_some());
    }

    /// Closed loop: Blender keeps GPU ~100% and cores ~84 degC; the case air heats toward
    /// 58 degC with fans at minimum and cools as the fans speed up (measured on the M1 Pro).
    /// Returns the first second the fans came on, the fan fraction and the case air each second.
    fn blender(c: &mut SmartCtl, secs: u32) -> (Option<u32>, Vec<f64>, Vec<f64>) {
        let (mut body, mut first, mut out, mut air) = (33.0, None, Vec::new(), Vec::new());
        for t in 0..secs {
            let s = Sample { battery: None, charging: false, chip: 84.0 + jitter(t), chassis: Some(32.0), body: Some(body), load: 0.99 };
            let f = c.step(&s, 60.0, 88.0).0;
            if f.is_some() && first.is_none() {
                first = Some(t);
            }
            let f = f.unwrap_or(0.0);
            out.push(f);
            air.push(body);
            let eq = 58.0 - 24.0 * f;
            body += (eq - body) / 120.0;
        }
        (first, out, air)
    }

    #[test]
    fn blender_keeps_pushing_until_the_case_cools() {
        // measured: the old curve settled at ~3000 rpm with the case air stuck at ~51 degC
        // and the bottom case getting hotter; the fans must keep stepping up until it comes down
        let mut c = SmartCtl::default();
        sim(&mut c, 60, idle);
        let (first, out, air) = blender(&mut c, 2400);
        let first = first.expect("Blender heat must start the fans");
        assert!(first <= 240, "started too late at {first}s");
        for t in (0..out.len()).step_by(120) {
            println!("t={t:4}s target {:4.0} rpm  case air {:4.1}", 1499.0 + out[t] * (5348.0 - 1499.0), air[t]);
        }
        let late = &air[1800..];
        let avg = late.iter().sum::<f64>() / late.len() as f64;
        let rpm = 1499.0 + out.last().unwrap() * (5348.0 - 1499.0);
        println!("started at {first}s, settled {rpm:.0} rpm, case air {avg:.1}");
        assert!(avg <= BODY_TARGET + 1.5, "case air stayed at {avg:.1} degC");
        assert!(rpm > 3000.0 && rpm < 5348.0, "settled at {rpm:.0} rpm");
    }

    #[test]
    fn heat_that_will_not_drop_reaches_full_speed() {
        // a heat source the fans can't beat (hot room, blocked vents): keep climbing to max
        let mut c = SmartCtl::default();
        let (_, on) = sim(&mut c, 900, |_| Sample { battery: None, charging: false, chip: 80.0, chassis: Some(33.0), body: Some(52.0), load: 0.99 });
        assert!(on);
        let f = c.step(&Sample { battery: None, charging: false, chip: 80.0, chassis: Some(33.0), body: Some(52.0), load: 0.99 }, 60.0, 88.0).0.unwrap();
        assert!(f >= 0.99, "only at {f:.2} after 15 min at 52 degC case air");
    }

    #[test]
    fn speed_holds_steady_instead_of_hunting() {
        let mut c = SmartCtl::default();
        let (_, out, _) = blender(&mut c, 2700);
        // after settling (last 15 minutes) the target may change only a couple of times
        let changes = out[1800..].windows(2).filter(|w| (w[0] - w[1]).abs() > 1e-9).count();
        assert!(changes <= 2, "target changed {changes} times in 15 min");
    }

    #[test]
    fn steady_gpu_load_with_cool_case_stays_quiet() {
        let mut c = SmartCtl::default();
        let (first, _) = sim(&mut c, 1200, |t| Sample { battery: None, charging: false,
            chip: 70.0 + jitter(t) / 2.0,
            chassis: Some(30.0),
            body: Some(40.0 + jitter(t) / 4.0),
            load: 0.99,
        });
        assert_eq!(first, None);
    }

    #[test]
    fn epic_launcher_stays_quiet() {
        // launcher start/update: ~2 min of 75% load, cores 80-90 degC, palm rest barely moves
        let mut c = SmartCtl::default();
        sim(&mut c, 60, idle);
        let (first, _) = sim(&mut c, 400, |t| {
            if t < 130 {
                Sample { battery: None, charging: false, body: None,
                    chip: 85.0 + jitter(t),
                    chassis: Some(25.0 + t as f64 / 60.0),
                    load: 0.75 + jitter(t + 3) / 40.0,
                }
            } else {
                Sample { battery: None, charging: false, body: None,
                    chip: 50.0 + jitter(t),
                    chassis: Some(27.0),
                    load: 0.15,
                }
            }
        });
        assert_eq!(first, None, "a launcher starting up must not spin the fans");
    }

    #[test]
    fn busy_and_climbing_chip_cools_even_with_cool_chassis() {
        let mut c = SmartCtl::default();
        sim(&mut c, 60, idle);
        let (first, on) = sim(&mut c, 600, |t| Sample { battery: None, charging: false, body: None,
            chip: (55.0 + 0.1 * t as f64).min(85.0) + jitter(t),
            chassis: Some(30.0),
            load: 0.9,
        });
        assert!(first.is_some() && on);
    }

    #[test]
    fn warm_chassis_cools() {
        let mut c = SmartCtl::default();
        let (first, _) = sim(&mut c, 60, |_| Sample { battery: None, charging: false, body: None,
            chip: 55.0,
            chassis: Some(39.0),
            load: 0.1,
        });
        assert!(first.is_some());
    }

    #[test]
    fn hands_back_only_after_a_calm_minute() {
        let mut c = SmartCtl::default();
        let (_, on) = sim(&mut c, 200, |_| Sample { battery: None, charging: false, body: None,
            chip: 78.0,
            chassis: Some(36.0),
            load: 0.9,
        });
        assert!(on);
        // work stops; chip cools over ~40 s, chassis cools
        let mut off_at = None;
        for t in 0..400u32 {
            let s = Sample { battery: None, charging: false, body: None,
                chip: (78.0 - 0.75 * t as f64).max(46.0),
                chassis: Some((36.0 - 0.05 * t as f64).max(31.0)),
                load: 0.05,
            };
            if c.step(&s, 60.0, 88.0).0.is_none() {
                off_at = Some(t);
                break;
            }
        }
        let off_at = off_at.expect("fans never handed back");
        assert!(off_at >= STOP_SECS, "handed back too early at {off_at}s");
    }

    #[test]
    fn hands_back_when_heat_is_gone_even_with_background_load() {
        // measured on the M1 Pro: after work, background load hovered 30-40%, chip ~52 degC and falling
        let mut c = SmartCtl::default();
        let (_, on) = sim(&mut c, 500, |t| Sample { battery: None, charging: false, body: None,
            chip: (48.0 + 0.2 * t as f64).min(80.0),
            chassis: Some(29.0),
            load: 0.98,
        });
        assert!(on, "sustained heavy heating work should be cooling");
        let mut off_at = None;
        for t in 0..300u32 {
            let s = Sample { battery: None, charging: false, body: None,
                chip: (75.0 - 0.5 * t as f64).max(52.0),
                chassis: Some(29.0),
                load: 0.38,
            };
            if c.step(&s, 60.0, 88.0).0.is_none() {
                off_at = Some(t);
                break;
            }
        }
        let off_at = off_at.expect("fans stayed on after the heat was gone");
        assert!(
            (STOP_SECS..=200).contains(&off_at),
            "handed back at {off_at}s"
        );
    }

    /// Replays a "macsmartcooler watch" log (MACSMARTCOOLER_REPLAY=path) through the current logic.
    #[test]
    #[ignore]
    fn replay_log() {
        let path = std::env::var("MACSMARTCOOLER_REPLAY").expect("set MACSMARTCOOLER_REPLAY");
        let text = std::fs::read_to_string(path).unwrap();
        let mut c = SmartCtl::default();
        let num = |l: &str, key: &str| -> f64 {
            let i = l.find(key).unwrap() + key.len();
            l[i..]
                .trim_start()
                .split(|ch: char| !(ch.is_ascii_digit() || ch == '.' || ch == '-'))
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        let mut last = String::new();
        for l in text.lines() {
            let s = Sample { battery: None, charging: false, body: None,
                chip: num(l, "chip"),
                chassis: Some(num(l, "chassis")),
                load: (num(l, "cpu").max(num(l, "gpu"))) / 100.0,
            };
            let (f, why) = c.step(&s, 60.0, 88.0);
            let state = format!(
                "{} {}",
                f.map_or("OFF".into(), |f| format!("ON {:.0}%", f * 100.0)),
                why
            );
            let t = l.split('s').next().unwrap().trim();
            if state.split_whitespace().next() != last.split_whitespace().next()
                || l.starts_with(&format!("{:>4}", t)) && t.parse::<u32>().unwrap_or(1) % 30 == 0
            {
                println!(
                    "{t:>4}s chip {:4.1} load {:3.0}% -> {state}",
                    s.chip,
                    s.load * 100.0
                );
            }
            last = state;
        }
    }

    #[test]
    fn hotter_means_faster() {
        let mut c = SmartCtl::default();
        sim(&mut c, 60, |_| Sample { battery: None, charging: false, body: None,
            chip: 72.0,
            chassis: Some(36.0),
            load: 0.6,
        });
        let a = c
            .step(
                &Sample { battery: None, charging: false, body: None,
                    chip: 72.0,
                    chassis: Some(36.0),
                    load: 0.6,
                },
                60.0,
                88.0,
            )
            .0
            .unwrap();
        let mut d = SmartCtl::default();
        sim(&mut d, 60, |_| Sample { battery: None, charging: false, body: None,
            chip: 86.0,
            chassis: Some(36.0),
            load: 0.6,
        });
        let b = d
            .step(
                &Sample { battery: None, charging: false, body: None,
                    chip: 86.0,
                    chassis: Some(36.0),
                    load: 0.6,
                },
                60.0,
                88.0,
            )
            .0
            .unwrap();
        assert!(b > a && b <= 1.0, "{a} vs {b}");
    }

    #[test]
    fn charging_with_a_warm_battery_cools_even_when_idle() {
        // measured: charging at low load, battery 42 degC, palm rest and case air still cool
        let mut c = SmartCtl::default();
        sim(&mut c, 60, idle);
        let (first, on) = sim(&mut c, 600, |t| Sample {
            chip: 55.0,
            chassis: Some(31.0),
            body: Some(38.0),
            load: 0.15,
            battery: Some((33.0 + t as f64 / 40.0).min(42.0)),
            charging: true,
        });
        let first = first.expect("a battery warming while charging must start the fans");
        assert!(on);
        // starts once the battery passes 38 degC (~200 s) plus the 20 s hold
        assert!((200..=260).contains(&first), "started at {first}s");
    }

    #[test]
    fn warm_battery_without_charging_or_load_stays_quiet() {
        let mut c = SmartCtl::default();
        let (first, _) = sim(&mut c, 600, |_| Sample {
            chip: 50.0,
            chassis: Some(30.0),
            body: Some(36.0),
            load: 0.1,
            battery: Some(39.0),
            charging: false,
        });
        assert_eq!(first, None, "39 degC without charging is below the 'hot' line");
    }

    #[test]
    fn hot_battery_cools_whatever_else_is_happening() {
        let mut c = SmartCtl::default();
        let (first, _) = sim(&mut c, 60, |_| Sample {
            chip: 50.0,
            chassis: Some(30.0),
            body: Some(36.0),
            load: 0.05,
            battery: Some(42.0),
            charging: false,
        });
        assert!(first.is_some());
        let (_, why) = c.step(&Sample { chip: 50.0, chassis: Some(30.0), body: Some(36.0), load: 0.05, battery: Some(42.0), charging: false }, 60.0, 88.0);
        assert!(why.contains("battery"), "{why}");
    }

    #[test]
    fn hands_back_once_the_battery_has_cooled() {
        let mut c = SmartCtl::default();
        let warm = Sample { chip: 50.0, chassis: Some(30.0), body: Some(36.0), load: 0.1, battery: Some(42.0), charging: true };
        let (_, on) = sim(&mut c, 120, |_| warm);
        assert!(on);
        let cool = Sample { battery: Some(35.0), charging: false, ..warm };
        let (_, on) = sim(&mut c, 120, |_| cool);
        assert!(!on, "fans must go back to macOS once the battery is cool");
    }

    #[test]
    fn send_reports_a_dead_control_loop() {
        let (tx, rx) = channel();
        let w = Worker { snapshot: Default::default(), tx, handle: None };
        assert!(w.send(Cmd::SetMode(0, FanMode::Auto)));
        drop(rx);
        assert!(!w.send(Cmd::SetMode(0, FanMode::Auto)), "a mode change must not be reported as applied");
    }

    #[test]
    fn mode_roundtrip() {
        for m in [
            FanMode::Auto,
            FanMode::Constant(3123.5),
            FanMode::Smart { lo: 60.0, hi: 88.0 },
            FanMode::Sensor {
                key: "max:hot".into(),
                lo: 45.5,
                hi: 80.0,
            },
        ] {
            assert_eq!(FanMode::decode(&m.encode()), Some(m));
        }
        // values that would reach the fan controller unchecked
        for bad in ["const,NaN", "const,inf", "smart,NaN,88", "smart,60,-inf", "sensor,max:hot,NaN,80", "sensor,a b,40,80", "const,1e400"] {
            assert_eq!(FanMode::decode(bad), None, "{bad} should be rejected");
        }
        assert_eq!(FanMode::decode("smart,0,9999"), Some(FanMode::Smart { lo: 45.0, hi: 100.0 }));
    }

    /// The unprivileged app must mirror the helper's state and forward commands to it.
    #[test]
    fn app_talks_to_helper() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let sock = std::env::temp_dir().join(format!("mbt{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        std::env::set_var("MACSMARTCOOLER_SOCK", &sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let state = Arc::new(Mutex::new(vec!["auto".to_string(); 8]));
        let st = state.clone();
        std::thread::spawn(move || {
            for s in listener.incoming() {
                let mut s = s.unwrap();
                let mut line = String::new();
                BufReader::new(&s).read_line(&mut line).unwrap();
                let line = line.trim();
                let reply = if line == "get" {
                    format!("{}\t\tCooling: test", st.lock().unwrap().join("|"))
                } else if let Some(rest) = line.strip_prefix("set ") {
                    let (i, enc) = rest.split_once(' ').unwrap();
                    st.lock().unwrap()[i.parse::<usize>().unwrap()] = enc.to_string();
                    "ok".into()
                } else {
                    "bad command".into()
                };
                writeln!(s, "{reply}").unwrap();
            }
        });

        let w = Worker::spawn(|| {});
        let wait = |cond: &dyn Fn(&Snapshot) -> bool| {
            for _ in 0..100 {
                if cond(&w.snapshot.lock().unwrap()) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            false
        };
        assert!(
            wait(&|s| s.ready && s.can_control),
            "app never saw the helper"
        );
        let n = w.snapshot.lock().unwrap().fans.len();
        assert!(n > 0, "no fans read");
        w.send(Cmd::SetMode(0, FanMode::Smart { lo: 60.0, hi: 88.0 }));
        assert!(
            wait(&|s| s.modes.first() == Some(&FanMode::Smart { lo: 60.0, hi: 88.0 })),
            "mode change did not round-trip"
        );
        assert_eq!(state.lock().unwrap()[0], "smart,60,88");
        assert_eq!(
            w.snapshot.lock().unwrap().activity.smart.as_deref(),
            Some("Cooling: test")
        );
        assert!(w.snapshot.lock().unwrap().error.is_none());

        // switching a preset (every fan at once) must show up quickly, not a loop per fan
        let t = std::time::Instant::now();
        for i in 0..n {
            w.send(Cmd::SetMode(i, FanMode::Auto));
        }
        assert!(
            wait(&|s| s.modes.iter().all(|m| *m == FanMode::Auto)),
            "preset never applied"
        );
        let took = t.elapsed();
        assert!(
            took < Duration::from_millis(400),
            "preset switch took {took:?}"
        );
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn slope_per_step() {
        assert!((slope(&[50.0, 51.0, 52.0, 53.0]) - 1.0).abs() < 1e-9);
    }
}
