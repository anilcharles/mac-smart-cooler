//! Which processes are using the CPU and GPU, and what was running when the fans came on.
//!
//! Display only: the background helper and Smart Cooling never use any of this (and this
//! file is not part of the helper's version hash in build.rs).
//!
//! CPU time comes from `/bin/ps`, which macOS installs setuid root: an unprivileged app can
//! read the CPU time of its own processes only, and the ones that heat a Mac are often system
//! processes (WindowServer, Spotlight's mds_stores, backupd, kernel_task). GPU time comes from
//! the GPU's IORegistry entries, which any account may read. Both are running totals; usage is
//! the difference between two samples divided by the time between them.

use std::collections::{HashMap, VecDeque};
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

/// Usage shown in the table is averaged over this many seconds (Smart Cooling's "sustained").
pub const TABLE_SECS: f64 = 30.0;
/// What ran in the minute before the fans came on.
pub const BLAME_SECS: f64 = 60.0;
/// How much history is kept.
const KEEP_SECS: f64 = 90.0;
/// Sample every 2 s while the table is on screen, every 10 s otherwise (to catch spin-ups).
const FAST: Duration = Duration::from_secs(2);
const SLOW: Duration = Duration::from_secs(10);
/// Rows kept for the table.
pub const MAX_ROWS: usize = 40;

/// One process as read: running totals.
#[derive(Clone, Debug, PartialEq)]
pub struct Raw {
    pub pid: i32,
    pub uid: u32,
    /// CPU time used since the process started, seconds
    pub cpu_s: f64,
    /// GPU time used since the process started, nanoseconds (0 when unknown)
    pub gpu_ns: u64,
    pub name: String,
}

/// Who a process runs as (shown in the table's tooltip).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    You,
    System,
    OtherUser,
}

/// One row of the table: every process with this name, added up.
#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub name: String,
    pub pids: Vec<i32>,
    /// CPU, as Activity Monitor counts it: 1.0 = one core fully busy (can exceed 1.0)
    pub cpu: f64,
    /// share of the GPU, 0..1
    pub gpu: f64,
    pub owner: Owner,
}

/// The latest table, as the window reads it.
#[derive(Clone, Debug, Default)]
pub struct Latest {
    /// averaged over TABLE_SECS, busiest first (by CPU)
    pub table: Vec<Usage>,
    /// averaged over BLAME_SECS, busiest first: what a fan spin-up is blamed on
    pub minute: Vec<Usage>,
    /// this Mac reports per-process GPU time (Apple silicon does)
    pub gpu_known: bool,
    /// seconds of history behind `table` (0 until two samples exist)
    pub span: f64,
    pub error: Option<String>,
}

/* ------------------------------------------------------------------ reading */

/// "12:34.56", "1234:56.78" or "1-02:03:04.50" (as ps prints CPU time) to seconds.
fn parse_cputime(s: &str) -> Option<f64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().ok()?, r),
        None => (0.0, s),
    };
    let mut secs = 0.0;
    for part in rest.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)?;
    }
    Some(days * 86_400.0 + secs)
}

/// Lines of `ps -Aco pid=,uid=,time=,comm=`. The name is last because it may contain spaces.
fn parse_ps(out: &str) -> Vec<Raw> {
    out.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let uid = it.next()?.parse().ok()?;
            let cpu_s = parse_cputime(it.next()?)?;
            let name = it.collect::<Vec<_>>().join(" ");
            (!name.is_empty()).then_some(Raw { pid, uid, cpu_s, gpu_ns: 0, name })
        })
        .collect()
}

fn read_ps() -> Result<Vec<Raw>, String> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-Aco", "pid=,uid=,time=,comm="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("couldn't run ps: {e}"))?;
    if !out.status.success() {
        return Err("ps failed".into());
    }
    Ok(parse_ps(&String::from_utf8_lossy(&out.stdout)))
}

/// "pid 194, WindowServer" (an IOUserClientCreator value) to 194.
fn creator_pid(s: &str) -> Option<i32> {
    s.strip_prefix("pid ")?.split(',').next()?.trim().parse().ok()
}

type CFTypeRef = *const c_void;
const UTF8: u32 = 0x0800_0100;
const CF_NUMBER_SINT64: i32 = 4;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, enc: u32) -> CFTypeRef;
    fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, len: isize, enc: u32) -> bool;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDictionaryGetValue(d: CFTypeRef, k: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(n: CFTypeRef, t: i32, out: *mut c_void) -> bool;
    fn CFGetTypeID(x: CFTypeRef) -> usize;
    fn CFRelease(x: CFTypeRef);
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main: u32, matching: *mut c_void, iter: *mut u32) -> i32;
    fn IORegistryEntryGetChildIterator(entry: u32, plane: *const c_char, iter: *mut u32) -> i32;
    fn IOIteratorNext(iter: u32) -> u32;
    fn IOObjectRelease(o: u32) -> i32;
    fn IORegistryEntryCreateCFProperty(entry: u32, key: CFTypeRef, alloc: CFTypeRef, opts: u32) -> CFTypeRef;
}

/// A CFString key, released on drop.
struct Key(CFTypeRef);
impl Key {
    fn new(s: &str) -> Option<Key> {
        let c = CString::new(s).ok()?;
        let k = unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) };
        (!k.is_null()).then_some(Key(k))
    }
}
impl Drop for Key {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

/// GPU time per process (nanoseconds), from the GPU's user clients: each one names the
/// process that opened it and the GPU time it has used. None when this Mac doesn't report it.
fn read_gpu() -> Option<HashMap<i32, u64>> {
    let (k_creator, k_usage, k_time) = (Key::new("IOUserClientCreator")?, Key::new("AppUsage")?, Key::new("accumulatedGPUTime")?);
    let class = CString::new("IOAccelerator").ok()?;
    let plane = CString::new("IOService").ok()?;
    let mut out: HashMap<i32, u64> = HashMap::new();
    let mut seen_usage = false;
    unsafe {
        let mut accels = 0u32;
        if IOServiceGetMatchingServices(0, IOServiceMatching(class.as_ptr()), &mut accels) != 0 {
            return None;
        }
        loop {
            let accel = IOIteratorNext(accels);
            if accel == 0 {
                break;
            }
            let mut children = 0u32;
            if IORegistryEntryGetChildIterator(accel, plane.as_ptr(), &mut children) == 0 {
                loop {
                    let child = IOIteratorNext(children);
                    if child == 0 {
                        break;
                    }
                    let usage = IORegistryEntryCreateCFProperty(child, k_usage.0, std::ptr::null(), 0);
                    if !usage.is_null() {
                        if CFGetTypeID(usage) == CFArrayGetTypeID() {
                            seen_usage = true;
                            let creator = IORegistryEntryCreateCFProperty(child, k_creator.0, std::ptr::null(), 0);
                            let pid = (!creator.is_null() && CFGetTypeID(creator) == CFStringGetTypeID())
                                .then(|| {
                                    let mut buf = [0 as c_char; 128];
                                    CFStringGetCString(creator, buf.as_mut_ptr(), buf.len() as isize, UTF8)
                                        .then(|| CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
                                })
                                .flatten()
                                .and_then(|s| creator_pid(&s));
                            if !creator.is_null() {
                                CFRelease(creator);
                            }
                            if let Some(pid) = pid {
                                let mut total = 0u64;
                                for i in 0..CFArrayGetCount(usage).max(0) {
                                    let d = CFArrayGetValueAtIndex(usage, i);
                                    if d.is_null() || CFGetTypeID(d) != CFDictionaryGetTypeID() {
                                        continue;
                                    }
                                    let n = CFDictionaryGetValue(d, k_time.0);
                                    let mut v: i64 = 0;
                                    if !n.is_null()
                                        && CFGetTypeID(n) == CFNumberGetTypeID()
                                        && CFNumberGetValue(n, CF_NUMBER_SINT64, &mut v as *mut i64 as *mut c_void)
                                    {
                                        total = total.saturating_add(v.max(0) as u64);
                                    }
                                }
                                *out.entry(pid).or_default() += total;
                            }
                        }
                        CFRelease(usage);
                    }
                    IOObjectRelease(child);
                }
                IOObjectRelease(children);
            }
            IOObjectRelease(accel);
        }
        IOObjectRelease(accels);
    }
    seen_usage.then_some(out)
}

/// Every process with its running CPU and GPU totals. The bool: GPU time is known.
pub fn read_all() -> Result<(Vec<Raw>, bool), String> {
    let mut procs = read_ps()?;
    let gpu = read_gpu();
    if let Some(g) = &gpu {
        for p in &mut procs {
            p.gpu_ns = g.get(&p.pid).copied().unwrap_or(0);
        }
    }
    Ok((procs, gpu.is_some()))
}

/* ------------------------------------------------------------------ usage over time */

extern "C" {
    fn getuid() -> u32;
}

/// The account this app runs as.
pub fn my_uid() -> u32 {
    unsafe { getuid() }
}

fn owner_of(uid: u32, me: u32) -> Owner {
    if uid == me {
        Owner::You
    } else if uid < 500 {
        Owner::System
    } else {
        Owner::OtherUser
    }
}

/// Recent samples, and usage worked out from them.
#[derive(Default)]
pub struct Tracker {
    samples: VecDeque<(f64, HashMap<i32, Raw>)>,
}

impl Tracker {
    /// Adds a sample taken `t` seconds into the run.
    pub fn push(&mut self, t: f64, procs: Vec<Raw>) {
        self.samples.push_back((t, procs.into_iter().map(|p| (p.pid, p)).collect()));
        while self.samples.front().is_some_and(|(t0, _)| t - t0 > KEEP_SECS) {
            self.samples.pop_front();
        }
    }

    /// Seconds of history available for an average over (up to) `span` seconds.
    pub fn span(&self, span: f64) -> f64 {
        match (self.base(span), self.samples.back()) {
            (Some((tb, _)), Some((tl, _))) => tl - tb,
            _ => 0.0,
        }
    }

    /// The oldest sample no more than `span` seconds before the newest.
    fn base(&self, span: f64) -> Option<&(f64, HashMap<i32, Raw>)> {
        let tl = self.samples.back()?.0;
        self.samples.iter().find(|(t, _)| tl - t <= span + 0.5)
    }

    /// Average usage per process name over the last `span` seconds, busiest first by CPU.
    /// A process missing from the older sample started since, so all its time is recent.
    pub fn usage(&self, span: f64, me: u32) -> Vec<Usage> {
        let (Some((tb, base)), Some((tl, last))) = (self.base(span), self.samples.back()) else { return Vec::new() };
        let dt = tl - tb;
        if dt <= 0.0 {
            return Vec::new();
        }
        let mut rows: HashMap<&str, Usage> = HashMap::new();
        for p in last.values() {
            let b = base.get(&p.pid);
            // a pid reused by a new process (its totals went down) starts from zero
            let cpu0 = b.map(|b| b.cpu_s).filter(|v| *v <= p.cpu_s).unwrap_or(0.0);
            let gpu0 = b.map(|b| b.gpu_ns).filter(|v| *v <= p.gpu_ns).unwrap_or(0);
            let cpu = (p.cpu_s - cpu0) / dt;
            let gpu = (p.gpu_ns - gpu0) as f64 / 1e9 / dt;
            let owner = owner_of(p.uid, me);
            let row = rows.entry(p.name.as_str()).or_insert_with(|| Usage {
                name: p.name.clone(),
                pids: Vec::new(),
                cpu: 0.0,
                gpu: 0.0,
                owner,
            });
            row.pids.push(p.pid);
            row.cpu += cpu;
            row.gpu += gpu;
            if owner == Owner::You {
                row.owner = Owner::You; // same name in several accounts: say it's yours
            }
        }
        let mut rows: Vec<Usage> = rows
            .into_values()
            .map(|mut u| {
                u.pids.sort_unstable();
                u.gpu = u.gpu.min(1.0);
                u
            })
            .filter(|u| u.cpu >= 0.001 || u.gpu >= 0.001)
            .collect();
        sort_rows(&mut rows, Sort::Cpu);
        rows
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    Cpu,
    Gpu,
}

/// Busiest first by the chosen column; ties by the other column, then by name.
pub fn sort_rows(rows: &mut [Usage], by: Sort) {
    let key = |u: &Usage| match by {
        Sort::Cpu => (u.cpu, u.gpu),
        Sort::Gpu => (u.gpu, u.cpu),
    };
    rows.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        kb.0.total_cmp(&ka.0).then(kb.1.total_cmp(&ka.1)).then_with(|| a.name.cmp(&b.name))
    });
}

/* ------------------------------------------------------------------ background sampler */

/// Samples processes on its own thread (running ps takes ~30 ms: never on the UI thread).
pub struct Sampler {
    latest: Arc<Mutex<Latest>>,
    watched: Arc<AtomicBool>,
}

impl Sampler {
    pub fn spawn() -> Self {
        let latest = Arc::new(Mutex::new(Latest::default()));
        let watched = Arc::new(AtomicBool::new(false));
        let (weak, flag) = (Arc::downgrade(&latest), watched.clone());
        std::thread::Builder::new()
            .name("processes".into())
            .spawn(move || run(weak, flag))
            .ok();
        Sampler { latest, watched }
    }

    pub fn latest(&self) -> Latest {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The table is on screen: sample often for a while.
    pub fn watch(&self) {
        self.watched.store(true, Ordering::Relaxed);
    }
}

fn run(latest: Weak<Mutex<Latest>>, watched: Arc<AtomicBool>) {
    let me = my_uid();
    let start = Instant::now();
    let mut tracker = Tracker::default();
    let mut last_watch: Option<Instant> = None;
    loop {
        let (sample, gpu_known) = match read_all() {
            Ok(r) => (Some(r.0), r.1),
            Err(e) => {
                let Some(l) = latest.upgrade() else { return };
                l.lock().unwrap_or_else(|e| e.into_inner()).error = Some(e);
                (None, false)
            }
        };
        if let Some(procs) = sample {
            tracker.push(start.elapsed().as_secs_f64(), procs);
            let mut table = tracker.usage(TABLE_SECS, me);
            table.truncate(MAX_ROWS);
            let mut minute = tracker.usage(BLAME_SECS, me);
            minute.truncate(MAX_ROWS);
            let next = Latest { table, minute, gpu_known, span: tracker.span(TABLE_SECS), error: None };
            let Some(l) = latest.upgrade() else { return };
            *l.lock().unwrap_or_else(|e| e.into_inner()) = next;
        }
        // wait, but wake early to speed up as soon as the table comes on screen
        let began = Instant::now();
        loop {
            if watched.swap(false, Ordering::Relaxed) {
                last_watch = Some(Instant::now());
            }
            // the first rates need two samples: take the second one soon
            let fast = tracker.samples.len() < 2 || last_watch.is_some_and(|t| t.elapsed() < Duration::from_secs(5));
            if began.elapsed() >= if fast { FAST } else { SLOW } {
                break;
            }
            if latest.strong_count() == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/* ------------------------------------------------------------------ fan spin-ups */

/// One time the fans came on, and what was busy just before.
#[derive(Clone, Debug)]
pub struct FanEvent {
    pub wall: SystemTime,
    pub started: Instant,
    /// how long the fans stayed on; None while they still are
    pub lasted: Option<Duration>,
    pub peak_rpm: f64,
    /// Smart Cooling's reason, when it was the one that turned them on
    pub why: Option<String>,
    /// the busiest processes over the minute before (or, for `at_launch`, while on)
    pub culprits: Vec<Usage>,
    /// the fans were already on when Mac Smart Cooler started: nothing is known from before,
    /// so `culprits` is what kept them busy during the first minute
    pub at_launch: bool,
}

/// Spots the fans coming on. A fan counts as "on" when it runs 500 rpm above the slowest it
/// has been seen, starting from its rated minimum (fans that stop at idle, as on Apple
/// silicon: above 500 rpm once seen stopped; fans that never stop, as on Intel Macs: well
/// above their idle speed).
#[derive(Default)]
pub struct SpinWatch {
    floor: Vec<f64>,
    calm_since: Option<Instant>,
    /// newest first
    pub events: VecDeque<FanEvent>,
}

const ON_ABOVE_FLOOR: f64 = 500.0;
const HYSTERESIS: f64 = 200.0;
/// The fans must stay down this long before the episode counts as over.
const CALM: Duration = Duration::from_secs(20);
const MAX_EVENTS: usize = 20;
/// Culprits shown per event.
pub const CULPRITS: usize = 3;

impl SpinWatch {
    /// `fans`: each fan's (current rpm, rated minimum rpm). `culprits`: the busiest processes
    /// over the last minute, asked for only when needed.
    pub fn update(
        &mut self,
        now: Instant,
        wall: SystemTime,
        fans: &[(f64, f64)],
        why: Option<&str>,
        culprits: impl FnOnce() -> Vec<Usage>,
    ) {
        if fans.is_empty() {
            return;
        }
        let first = self.floor.len() != fans.len();
        if first {
            self.floor = fans.iter().map(|(_, min)| if min.is_finite() { min.max(0.0) } else { 0.0 }).collect();
        }
        for (f, (r, _)) in self.floor.iter_mut().zip(fans) {
            *f = f.min(r.max(0.0));
        }
        let over = |slack: f64| fans.iter().zip(&self.floor).any(|((r, _), f)| *r > f + ON_ABOVE_FLOOR - slack);
        let peak = fans.iter().map(|(r, _)| *r).fold(0.0, f64::max);
        let open = self.events.front_mut().filter(|e| e.lasted.is_none());
        match open {
            Some(e) => {
                e.peak_rpm = e.peak_rpm.max(peak);
                let age = now.duration_since(e.started);
                if e.why.is_none() && age < Duration::from_secs(30) {
                    e.why = cooling_reason(why);
                }
                // already on at launch: keep refining the culprits over the first minute
                if e.at_launch && age <= Duration::from_secs_f64(BLAME_SECS + 10.0) {
                    e.culprits = culprits();
                    e.culprits.truncate(CULPRITS);
                }
                if over(HYSTERESIS) {
                    self.calm_since = None;
                } else {
                    let since = *self.calm_since.get_or_insert(now);
                    if now.duration_since(since) >= CALM {
                        e.lasted = Some(since.duration_since(e.started));
                        self.calm_since = None;
                    }
                }
            }
            None if over(0.0) => {
                let mut culprits = culprits();
                culprits.truncate(CULPRITS);
                self.events.push_front(FanEvent {
                    wall,
                    started: now,
                    lasted: None,
                    peak_rpm: peak,
                    why: cooling_reason(why),
                    culprits,
                    at_launch: first,
                });
                self.events.truncate(MAX_EVENTS);
                self.calm_since = None;
            }
            None => {}
        }
    }
}

/// Smart Cooling's reason ("Cooling: back of the case is warm") when it is cooling for one.
/// "winding down" and "pushing harder" describe what it is doing, not why, so they wait.
fn cooling_reason(why: Option<&str>) -> Option<String> {
    let (state, reason) = why?.split_once(": ")?;
    let not_a_cause = reason.is_empty() || reason.starts_with("winding down") || reason.contains("pushing harder");
    (state.starts_with("Cooling") && !not_a_cause).then(|| reason.to_string())
}

/* ------------------------------------------------------------------ plain-English names */

/// What a well-known process is, for processes whose names don't say.
pub fn explain(name: &str) -> Option<&'static str> {
    Some(match name {
        "kernel_task" => "The macOS kernel. Its share also rises when macOS holds the CPU back to cool down",
        "WindowServer" => "Draws everything on screen: busy with animations, video, many windows or external displays",
        "mds" | "mds_stores" | "mdworker" | "mdworker_shared" | "mdsync" => "Spotlight, indexing files so they can be searched",
        "backupd" | "backupd-helper" => "Time Machine, making a backup",
        "photoanalysisd" | "photolibraryd" | "mediaanalysisd" => "Photos, analysing your library (faces, scenes, text)",
        "bird" | "cloudd" | "fileproviderd" => "iCloud, syncing files",
        "softwareupdated" | "softwareupdate_download_service" => "macOS Software Update",
        "XprotectService" | "XProtect" | "syspolicyd" | "trustd" => "macOS security checks on apps and downloads",
        "com.apple.Virtualization.VirtualMachine" => "A virtual machine (Docker, a Linux VM or a sandbox)",
        "coreaudiod" => "Sound for every app",
        "launchd" => "Starts and stops macOS services",
        "corespotlightd" | "spotlightknowledged" => "Spotlight suggestions",
        "suggestd" | "knowledge-agent" | "biomesyncd" | "BiomeAgent" => "Siri and system suggestions, learning in the background",
        "nsurlsessiond" => "Background downloads for apps and macOS",
        "VTDecoderXPCService" | "VTEncoderXPCService" => "Video decoding or encoding for an app",
        "Safari Web Content" | "com.apple.WebKit.WebContent" => "A web page open in Safari or another app",
        "Google Chrome Helper (Renderer)" | "Google Chrome Helper" => "A tab or extension in Chrome",
        "Code Helper (Renderer)" | "Code Helper" | "Code Helper (Plugin)" => "Visual Studio Code: a window or extension",
        "plugin-container" => "A Firefox tab or extension",
        "Firefox GPU Helper" => "Firefox, drawing pages with the GPU",
        _ => return None,
    })
}

/* ------------------------------------------------------------------ formatting */

/// CPU as Activity Monitor shows it: "380%", "12%", "0.4%".
pub fn cpu_text(cpu: f64) -> String {
    let p = cpu * 100.0;
    if p < 1.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

/// GPU share: "41%", "0.6%", "–" for none.
pub fn gpu_text(gpu: f64) -> String {
    let p = gpu * 100.0;
    if p < 0.05 {
        "–".into()
    } else if p < 1.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

/// "just now", "4 min ago", "2 h ago", "3 days ago".
pub fn ago(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}

/// "45 s", "4 min", "1 h 20 min".
pub fn duration_text(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        let m = (s % 3600) / 60;
        if m == 0 {
            format!("{} h", s / 3600)
        } else {
            format!("{} h {m} min", s / 3600)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pid: i32, name: &str, cpu_s: f64, gpu_ns: u64) -> Raw {
        Raw { pid, uid: 501, cpu_s, gpu_ns, name: name.into() }
    }

    #[test]
    fn parses_ps_cpu_times() {
        assert_eq!(parse_cputime("0:00.00"), Some(0.0));
        assert_eq!(parse_cputime("22:46.61"), Some(22.0 * 60.0 + 46.61));
        assert_eq!(parse_cputime("1234:56.78"), Some(1234.0 * 60.0 + 56.78));
        assert_eq!(parse_cputime("1:02:03.50"), Some(3723.5));
        assert_eq!(parse_cputime("2-01:00:00"), Some(2.0 * 86_400.0 + 3600.0));
        assert_eq!(parse_cputime("abc"), None);
        assert_eq!(parse_cputime(""), None);
    }

    #[test]
    fn parses_ps_lines_with_spaces_in_names() {
        let out = "  194    88  22:46.61 WindowServer\n 1295   501   5:19.83 Code Helper (Renderer)\ngarbage\n   12 0 0:01.00\n";
        let p = parse_ps(out);
        assert_eq!(p.len(), 2, "lines without a name or with bad fields are skipped");
        assert_eq!(p[0], Raw { pid: 194, uid: 88, cpu_s: 22.0 * 60.0 + 46.61, gpu_ns: 0, name: "WindowServer".into() });
        assert_eq!(p[1].name, "Code Helper (Renderer)");
    }

    #[test]
    fn reads_the_pid_of_a_gpu_client() {
        assert_eq!(creator_pid("pid 194, WindowServer"), Some(194));
        assert_eq!(creator_pid("pid 724, Firefox GPU Help"), Some(724));
        assert_eq!(creator_pid("WindowServer"), None);
    }

    #[test]
    fn usage_is_the_difference_over_time() {
        let mut t = Tracker::default();
        t.push(0.0, vec![raw(1, "Blender", 100.0, 0), raw(2, "WindowServer", 50.0, 10_000_000_000)]);
        assert!(t.usage(30.0, 501).is_empty(), "one sample is not enough");
        t.push(10.0, vec![raw(1, "Blender", 138.0, 0), raw(2, "WindowServer", 51.0, 12_000_000_000)]);
        let u = t.usage(30.0, 501);
        assert_eq!(u[0].name, "Blender");
        assert!((u[0].cpu - 3.8).abs() < 1e-9, "38 s of CPU in 10 s = 380%");
        assert!((u[1].cpu - 0.1).abs() < 1e-9);
        assert!((u[1].gpu - 0.2).abs() < 1e-9, "2 s of GPU in 10 s = 20%");
    }

    #[test]
    fn a_new_process_counts_from_zero_and_a_reused_pid_resets() {
        let mut t = Tracker::default();
        t.push(0.0, vec![raw(7, "old", 500.0, 0)]);
        // pid 7 is now a different, younger process; pid 8 started since
        t.push(10.0, vec![raw(7, "new", 2.0, 0), raw(8, "fresh", 5.0, 0)]);
        let u = t.usage(30.0, 501);
        let get = |n: &str| u.iter().find(|x| x.name == n).map(|x| x.cpu);
        assert_eq!(get("fresh"), Some(0.5));
        assert_eq!(get("new"), Some(0.2), "never negative, never the old process's time");
    }

    #[test]
    fn processes_with_the_same_name_are_added_up() {
        let mut t = Tracker::default();
        t.push(0.0, vec![raw(3, "Code Helper", 0.0, 0), raw(4, "Code Helper", 0.0, 0), raw(5, "idle", 1.0, 0)]);
        t.push(10.0, vec![raw(3, "Code Helper", 1.0, 0), raw(4, "Code Helper", 2.0, 0), raw(5, "idle", 1.0, 0)]);
        let u = t.usage(30.0, 501);
        assert_eq!(u.len(), 1, "processes that used nothing are left out");
        assert_eq!(u[0].pids, vec![3, 4]);
        assert!((u[0].cpu - 0.3).abs() < 1e-9);
    }

    #[test]
    fn averages_only_over_the_asked_span() {
        let mut t = Tracker::default();
        // busy for the first 60 s, then idle
        t.push(0.0, vec![raw(1, "job", 0.0, 0)]);
        t.push(60.0, vec![raw(1, "job", 60.0, 0)]);
        t.push(90.0, vec![raw(1, "job", 60.0, 0)]);
        assert!(t.usage(30.0, 501).is_empty(), "idle over the last 30 s");
        assert!((t.usage(90.0, 501)[0].cpu - 60.0 / 90.0).abs() < 1e-9);
        assert_eq!(t.span(30.0), 30.0);
    }

    #[test]
    fn old_samples_are_dropped() {
        let mut t = Tracker::default();
        for i in 0..100 {
            t.push(i as f64 * 2.0, vec![raw(1, "x", i as f64, 0)]);
        }
        assert!(t.samples.len() <= (KEEP_SECS / 2.0) as usize + 1);
    }

    #[test]
    fn owners() {
        assert_eq!(owner_of(501, 501), Owner::You);
        assert_eq!(owner_of(0, 501), Owner::System);
        assert_eq!(owner_of(88, 501), Owner::System);
        assert_eq!(owner_of(502, 501), Owner::OtherUser);
    }

    #[test]
    fn sorts_by_either_column() {
        let u = |n: &str, cpu, gpu| Usage { name: n.into(), pids: vec![], cpu, gpu, owner: Owner::You };
        let mut rows = vec![u("a", 0.1, 0.9), u("b", 2.0, 0.0), u("c", 0.1, 0.9)];
        sort_rows(&mut rows, Sort::Cpu);
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["b", "a", "c"]);
        sort_rows(&mut rows, Sort::Gpu);
        assert_eq!(rows[0].name, "a");
    }

    fn usage(name: &str) -> Vec<Usage> {
        vec![Usage { name: name.into(), pids: vec![1], cpu: 3.0, gpu: 0.0, owner: Owner::You }]
    }

    #[test]
    fn records_when_fans_that_stop_at_idle_come_on() {
        let t0 = Instant::now();
        let w = SystemTime::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut sw = SpinWatch::default();
        let f = |a: f64, b: f64| [(a, 1200.0), (b, 1200.0)];
        sw.update(at(0), w, &f(0.0, 0.0), None, || panic!("not asked while the fans are off"));
        sw.update(at(1), w, &f(400.0, 0.0), None, || panic!("400 rpm is not on"));
        assert!(sw.events.is_empty());
        sw.update(at(2), w, &f(1200.0, 1150.0), Some("Cooling: back of the case is warm"), || usage("Blender"));
        assert_eq!(sw.events.len(), 1);
        let e = &sw.events[0];
        assert_eq!(e.culprits[0].name, "Blender");
        assert_eq!(e.why.as_deref(), Some("back of the case is warm"));
        assert!(e.lasted.is_none());
        assert!(!e.at_launch);
        // spins up further, then dips for a moment: still the same episode
        sw.update(at(10), w, &f(2400.0, 2300.0), None, || panic!());
        sw.update(at(11), w, &f(0.0, 0.0), None, || panic!());
        sw.update(at(15), w, &f(1500.0, 1500.0), None, || panic!());
        assert_eq!(sw.events.len(), 1);
        assert_eq!(sw.events[0].peak_rpm, 2400.0);
        // off for good
        sw.update(at(40), w, &f(0.0, 0.0), None, || panic!());
        sw.update(at(70), w, &f(0.0, 0.0), None, || panic!());
        assert_eq!(sw.events[0].lasted, Some(Duration::from_secs(38)));
        // and a second time
        sw.update(at(100), w, &f(1300.0, 0.0), Some("Quiet: light work"), || usage("mds_stores"));
        assert_eq!(sw.events.len(), 2);
        assert_eq!(sw.events[0].culprits[0].name, "mds_stores");
        assert_eq!(sw.events[0].why, None, "only Smart Cooling's cooling reasons are kept");
        assert_eq!(cooling_reason(Some("Cooling: winding down")), None);
        assert_eq!(cooling_reason(Some("Cooling: heat is building up, pushing harder")), None);
        assert_eq!(cooling_reason(Some("Cooling: chip is hot")).as_deref(), Some("chip is hot"));
    }

    #[test]
    fn fans_that_never_stop_count_as_on_well_above_idle() {
        let t0 = Instant::now();
        let w = SystemTime::now();
        let mut sw = SpinWatch::default();
        sw.update(t0, w, &[(1300.0, 1200.0)], None, || panic!("idle speed"));
        sw.update(t0 + Duration::from_secs(1), w, &[(1600.0, 1200.0)], None, || panic!("a little above idle"));
        sw.update(t0 + Duration::from_secs(2), w, &[(2000.0, 1200.0)], None, || usage("kernel_task"));
        assert_eq!(sw.events.len(), 1);
    }

    #[test]
    fn fans_already_on_at_launch_are_recorded_and_blamed_on_the_first_minute() {
        let t0 = Instant::now();
        let w = SystemTime::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut sw = SpinWatch::default();
        // opened because the fans are loud: no process data yet
        sw.update(at(0), w, &[(4880.0, 1499.0), (5240.0, 1499.0)], None, Vec::new);
        assert_eq!(sw.events.len(), 1);
        assert!(sw.events[0].at_launch);
        assert!(sw.events[0].culprits.is_empty());
        sw.update(at(10), w, &[(4880.0, 1499.0), (5240.0, 1499.0)], None, || usage("VirtualMachine"));
        assert_eq!(sw.events[0].culprits[0].name, "VirtualMachine", "filled in once measured");
        sw.update(at(200), w, &[(4880.0, 1499.0), (5240.0, 1499.0)], None, || usage("later"));
        assert_eq!(sw.events[0].culprits[0].name, "VirtualMachine", "settled after the first minute");
        // fans at their minimum at launch are not "on"
        let mut quiet = SpinWatch::default();
        quiet.update(at(0), w, &[(1499.0, 1499.0)], None, || panic!("at minimum"));
        assert!(quiet.events.is_empty());
    }

    #[test]
    fn keeps_a_bounded_history() {
        let t0 = Instant::now();
        let w = SystemTime::now();
        let mut sw = SpinWatch::default();
        for i in 0..50u64 {
            sw.update(t0 + Duration::from_secs(i * 100), w, &[(2000.0, 1200.0)], None, Vec::new);
            sw.update(t0 + Duration::from_secs(i * 100 + 1), w, &[(0.0, 1200.0)], None, Vec::new);
            sw.update(t0 + Duration::from_secs(i * 100 + 30), w, &[(0.0, 1200.0)], None, Vec::new);
        }
        assert_eq!(sw.events.len(), MAX_EVENTS);
    }

    #[test]
    fn explains_common_system_processes() {
        assert!(explain("mds_stores").unwrap().contains("Spotlight"));
        assert!(explain("com.apple.Virtualization.VirtualMachine").is_some());
        assert_eq!(explain("Blender"), None);
    }

    #[test]
    fn formats() {
        assert_eq!(cpu_text(3.8), "380%");
        assert_eq!(cpu_text(0.004), "0.4%");
        assert_eq!(gpu_text(0.0), "–");
        assert_eq!(gpu_text(0.41), "41%");
        assert_eq!(ago(Duration::from_secs(5)), "just now");
        assert_eq!(ago(Duration::from_secs(250)), "4 min ago");
        assert_eq!(ago(Duration::from_secs(7300)), "2 h ago");
        assert_eq!(duration_text(Duration::from_secs(45)), "45 s");
        assert_eq!(duration_text(Duration::from_secs(4800)), "1 h 20 min");
    }

    /// Reads this Mac's processes for real (no root needed).
    #[test]
    fn reads_this_macs_processes() {
        let (procs, gpu) = read_all().expect("ps runs");
        assert!(procs.len() > 20, "ps lists every account's processes");
        assert!(procs.iter().any(|p| p.uid == 0), "including root's");
        assert!(procs.iter().any(|p| p.pid == std::process::id() as i32));
        println!("{} processes, per-process GPU time: {gpu}", procs.len());
    }
}
