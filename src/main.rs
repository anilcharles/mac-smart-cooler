mod auth;
mod charts;
mod elided;
mod fans;
mod helper;
mod load;
mod native;
#[cfg(debug_assertions)]
mod sample;
mod sensors;
mod settings;
mod smc;
#[cfg(debug_assertions)]
mod snapshot;
mod state;
mod theme;
mod tray;
mod ui;
mod ui_prefs;
mod widgets;
mod worker;

use smc::{is_root, Smc};

const USAGE: &str = "\
Mac Smart Cooler: Mac fan control & temperature monitor

USAGE:
    macsmartcooler                 open the GUI (use sudo to allow fan control)
    macsmartcooler list            print fans and temperature sensors
    macsmartcooler set <fan> <rpm> force fan (0-based index) to a fixed RPM  [root]
    macsmartcooler auto [fan]      return a fan (or all fans) to automatic   [root]
    macsmartcooler smart [lo] [hi] headless Smart Cooling until Ctrl-C       [root]
    macsmartcooler watch           print Smart Cooling's decision every second (changes nothing)
    macsmartcooler selftest        check on the real hardware that the helper really controls
                                   the fans (sets each to a fixed speed, then restores)
    macsmartcooler install-helper  install the background helper (asks for password once)
    macsmartcooler uninstall-helper remove the background helper
                                (default: cool when heading past 60 °C, full speed at 88 °C)
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if matches!(args.as_slice(), [] | ["smart", ..] | ["daemon"]) && is_root() {
        install_restore_handler();
    }
    let result = match args.as_slice() {
        [] => run_gui(),
        ["list"] => cli_list(),
        ["set", fan, rpm] => cli_set(fan, rpm),
        ["auto"] => with_smc(|s| {
            fans::restore_all(s);
            Ok(())
        }),
        ["auto", fan] => cli_auto(fan),
        ["smart", rest @ ..] if rest.len() <= 2 => cli_smart(rest),
        ["daemon"] => helper::serve(),
        ["watch"] => cli_watch(),
        ["selftest"] => cli_selftest(),
        ["__install-root", src] => helper::install_as_root(src),
        ["__uninstall-root"] => helper::uninstall_as_root(),
        ["install-helper"] => helper::install().map(|_| ()),
        ["uninstall-helper"] => helper::uninstall().map(|_| ()),
        ["-h" | "--help" | "help"] => {
            print!("{USAGE}");
            Ok(())
        }
        _ => Err(format!("unrecognised arguments\n\n{USAGE}")),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn with_smc(f: impl FnOnce(&mut Smc) -> Result<(), String>) -> Result<(), String> {
    let mut smc = Smc::open()?;
    f(&mut smc)
}

fn parse_fan(s: &str, smc: &mut Smc) -> Result<usize, String> {
    let i: usize = s.parse().map_err(|_| format!("bad fan index {s:?}"))?;
    let n = fans::fan_count(smc);
    if i >= n {
        return Err(format!(
            "fan index {i} out of range (this Mac has {n} fans)"
        ));
    }
    Ok(i)
}

fn cli_list() -> Result<(), String> {
    with_smc(|smc| {
        let n = fans::fan_count(smc);
        println!("Fans ({n}):");
        for i in 0..n {
            let f = fans::read_fan(smc, i, n);
            println!(
                "  [{i}] {:<12} min {:>5.0}  current {:>5.0}  max {:>5.0}  target {:>5.0}  {}",
                f.name,
                f.min,
                f.actual,
                f.max,
                f.target,
                if f.manual { "MANUAL" } else { "auto" }
            );
        }
        let keys = sensors::discover(smc);
        println!("\nTemperature sensors:");
        for r in sensors::read_all(smc, &keys) {
            println!("  {:<26} {:>6.1} °C   ({})", r.name, r.value, r.key);
        }
        Ok(())
    })
}

fn cli_set(fan: &str, rpm: &str) -> Result<(), String> {
    if !is_root() {
        return Err("setting fan speed requires root (sudo)".into());
    }
    with_smc(|smc| {
        let i = parse_fan(fan, smc)?;
        let n = fans::fan_count(smc);
        let f = fans::read_fan(smc, i, n);
        let rpm: f64 = rpm.parse().map_err(|_| format!("bad rpm {rpm:?}"))?;
        if !(f.max > f.min) {
            return Err("couldn't read this fan's min/max speed".into());
        }
        let rpm = rpm.clamp(f.min, f.max);
        fans::set_manual(smc, i, rpm)?;
        println!(
            "{} -> {rpm:.0} rpm (manual). Run 'sudo macsmartcooler auto' to restore.",
            f.name
        );
        Ok(())
    })
}

fn cli_auto(fan: &str) -> Result<(), String> {
    if !is_root() {
        return Err("changing fan mode requires root (sudo)".into());
    }
    with_smc(|smc| {
        let i = parse_fan(fan, smc)?;
        fans::set_auto(smc, i)
    })
}

extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

extern "C" fn restore_on_exit() {
    fans::restore_if_touched();
}

/// Never leave fans stuck in manual mode: covers normal quit, Cmd-Q (which calls exit()),
/// Ctrl-C, SIGTERM and SIGHUP.
fn install_restore_handler() {
    unsafe {
        atexit(restore_on_exit);
    }
    let _ = ctrlc::set_handler(|| std::process::exit(130));
}

fn cli_smart(rest: &[&str]) -> Result<(), String> {
    if !is_root() {
        return Err("smart cooling requires root (sudo)".into());
    }
    let num = |i: usize, d: f64| -> Result<f64, String> {
        rest.get(i).map_or(Ok(d), |s| {
            s.parse().map_err(|_| format!("bad temperature {s:?}"))
        })
    };
    let (lo, hi) = (num(0, 60.0)?, num(1, 88.0)?);
    let n = fans::fan_count(&mut Smc::open()?);
    let w = worker::Worker::spawn(|| {});
    for i in 0..n {
        w.send(worker::Cmd::SetMode(
            i,
            worker::FanMode::Smart {
                lo,
                hi: hi.max(lo + 5.0),
            },
        ));
    }
    println!("Smart Cooling on {n} fan(s): quiet during light work; cools for sustained load or when heading past {lo} °C (full speed at {hi} °C). Ctrl-C to stop.");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let s = w.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let hot = s
            .sensors
            .iter()
            .find(|r| r.key == sensors::HOT_KEY)
            .map_or(0.0, |r| r.value);
        let rpms: Vec<String> = s
            .fans
            .iter()
            .map(|f| format!("{} {:.0}", f.name, f.actual))
            .collect();
        let a = &s.activity;
        println!(
            "hottest {hot:.1} °C ({:+.1}/min) | cpu {:.0}% gpu {:.0}% (sustained {:.0}%) | {} | {}{}",
            a.trend,
            a.cpu * 100.0,
            a.gpu.unwrap_or(0.0) * 100.0,
            a.sustained * 100.0,
            a.smart.clone().unwrap_or_default(),
            rpms.join(" | "),
            s.error.map(|e| format!("  ⚠ {e}")).unwrap_or_default()
        );
    }
}

/// Icons are compiled into the binary.
struct Assets;

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../assets/icons/", $name, ".svg")) as &[u8])),*]
    };
}

const ICONS: &[(&str, &[u8])] = icons![
    "alert", "arrow-down", "arrow-right", "arrow-up", "auto", "battery", "check-bold", "check-circle", "chevron-down", "chip", "close",
    "fan", "gauge", "gear", "gpu", "laptop", "minus", "moon", "plus", "shield", "spinner", "sun", "thermometer", "trash",
    "wind",
];

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(name, _)| *name == path).map(|(_, data)| std::borrow::Cow::Borrowed(*data)))
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<gpui::SharedString>> {
        Ok(ICONS.iter().map(|(name, _)| gpui::SharedString::from(*name)).collect())
    }
}

/// Runs `f` on the app-wide state (menu actions that work with or without the window).
fn with_ctl(cx: &mut gpui::App, f: impl FnOnce(&mut state::Ctl, &mut gpui::Context<state::Ctl>)) {
    if let Some(ctl) = cx.try_global::<state::Shared>().map(|s| s.0.clone()) {
        ctl.update(cx, f);
    }
}

fn run_gui() -> Result<(), String> {
    use gpui::{Application, KeyBinding, Menu, MenuItem};
    use state::Preset;
    use ui::*;
    // Debug builds only: the snapshot walkthrough (see `ui::Main::start_demo`).
    let snapshots = cfg!(debug_assertions) && std::env::var_os("MACSMARTCOOLER_SNAPSHOT_DIR").is_some();
    let sample = snapshots && std::env::var_os("MACSMARTCOOLER_SAMPLE").is_some();

    let app = Application::new().with_assets(Assets);
    // Clicking the Dock icon brings the window back after it was closed.
    app.on_reopen(ui::open_window);
    app.run(move |cx: &mut gpui::App| {
        // SF Pro Rounded for the numbers, as in the original window (not a named system family,
        // so it is loaded from its file; without it the numbers use the system font).
        if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSRounded.ttf") {
            let _ = cx.text_system().add_fonts(vec![std::borrow::Cow::Owned(bytes)]);
        }
        cx.bind_keys([
            KeyBinding::new("escape", Cancel, None),
            KeyBinding::new("enter", Confirm, None),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-,", Preferences, None),
            KeyBinding::new("cmd-1", ModeAuto, None),
            KeyBinding::new("cmd-2", ModeSmart, None),
            KeyBinding::new("cmd-3", ModeFull, None),
            KeyBinding::new("cmd-w", CloseWindow, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("cmd-=", ZoomIn, None),
            KeyBinding::new("cmd-+", ZoomIn, None),
            KeyBinding::new("cmd--", ZoomOut, None),
            KeyBinding::new("cmd-0", ZoomReset, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &ModeAuto, cx| with_ctl(cx, |c, cx| c.choose_preset(Preset::Automatic, cx)));
        cx.on_action(|_: &ModeSmart, cx| with_ctl(cx, |c, cx| c.choose_preset(Preset::Smart, cx)));
        cx.on_action(|_: &ModeFull, cx| with_ctl(cx, |c, cx| c.choose_preset(Preset::FullBlast, cx)));
        cx.set_menus(vec![
            Menu {
                name: "Mac Smart Cooler".into(),
                items: vec![
                    MenuItem::action("About Mac Smart Cooler", About),
                    MenuItem::separator(),
                    MenuItem::action("Preferences…", Preferences),
                    MenuItem::separator(),
                    MenuItem::action("Quit Mac Smart Cooler", Quit),
                ],
            },
            Menu {
                name: "Fan".into(),
                items: vec![
                    MenuItem::action("Apple Default", ModeAuto),
                    MenuItem::action("Smart Cooling", ModeSmart),
                    MenuItem::action("Full Blast", ModeFull),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Light", ThemeLight),
                    MenuItem::action("Dark", ThemeDark),
                    MenuItem::action("Match System", ThemeAuto),
                    MenuItem::separator(),
                    MenuItem::action("Zoom In", ZoomIn),
                    MenuItem::action("Zoom Out", ZoomOut),
                    MenuItem::action("Actual Size", ZoomReset),
                ],
            },
            Menu {
                name: "Window".into(),
                items: vec![MenuItem::action("Minimize", Minimize), MenuItem::action("Close Window", CloseWindow)],
            },
        ]);
        // Closing the window does not quit: Mac Smart Cooler keeps running in the menu bar.
        state::start(cx, snapshots, sample);
        ui::open_window(cx);
    });
    Ok(())
}

/// Dry run of Smart Cooling on live sensors: prints what it would do, never touches the fans.
fn cli_watch() -> Result<(), String> {
    use std::io::Write;
    let mut smc = Smc::open()?;
    let keys = sensors::discover(&mut smc);
    let mut cpu = load::CpuSampler::new();
    let mut ctl = worker::SmartCtl::default();
    let s = settings::load();
    let start = std::time::Instant::now();
    loop {
        let readings = sensors::read_all(&mut smc, &keys);
        let chip = readings
            .iter()
            .find(|r| r.key == sensors::HOT_KEY)
            .map_or(0.0, |r| r.value);
        let chassis = readings
            .iter()
            .filter(|r| r.key == "Ts0P" || r.key == "Ts1P")
            .map(|r| r.value)
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
        let c = cpu.sample().unwrap_or(0.0);
        let g = load::gpu_utilization().unwrap_or(0.0);
        let (f, why) = ctl.step(
            &worker::Sample {
                chip,
                chassis,
                body: worker::case_air(&readings),
                load: c.max(g),
                battery: worker::battery_temp(&readings),
                charging: load::charging().unwrap_or(false),
            },
            s.smart_lo,
            s.smart_hi,
        );
        let line = format!(
            "{:>4}s chip {:5.1} case air {:5.1} chassis {:5.1} battery {:5.1}{} cpu {:3.0}% gpu {:3.0}% load~30s {:3.0}% trend {:+5.1}/min -> {} {}",
            start.elapsed().as_secs(),
            chip,
            worker::case_air(&readings).unwrap_or(0.0),
            chassis.unwrap_or(0.0),
            worker::battery_temp(&readings).unwrap_or(0.0),
            if load::charging() == Some(true) { " (charging)" } else { "" },
            c * 100.0,
            g * 100.0,
            ctl.load() * 100.0,
            ctl.trend(),
            f.map_or("OFF".to_string(), |f| format!("ON {:.0}%", f * 100.0)),
            why
        );
        if writeln!(std::io::stdout(), "{line}").is_err() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// End-to-end check on the real hardware: ask the installed helper to hold each fan at a
/// fixed speed, then read the SMC directly until every fan is really in manual mode near
/// that speed. Restores the previous modes afterwards, pass or fail. No sudo needed: the
/// person at the screen may change fans through the helper.
fn cli_selftest() -> Result<(), String> {
    use std::time::{Duration, Instant};
    let reply = helper::rpc("get").map_err(|e| format!("can't reach the helper: {e}"))?;
    let saved: Vec<String> = reply.split('\t').next().unwrap_or("").split('|').map(str::to_string).collect();
    let mut smc = Smc::open()?;
    let n = fans::fan_count(&mut smc);
    if n == 0 {
        println!("No fans on this Mac: nothing to test.");
        return Ok(());
    }
    let mut targets = Vec::new();
    for i in 0..n {
        let f = fans::read_fan(&mut smc, i, n);
        let rpm = ((f.min + 0.6 * (f.max - f.min)) / 50.0).round() * 50.0;
        targets.push(rpm);
        let r = helper::rpc(&format!("set {i} const,{rpm}"))?;
        println!("fan {i} ({}): asked the helper for {rpm:.0} rpm -> {r}", f.name);
    }
    let restore = |smc: &mut Smc| {
        for i in 0..n {
            let m = saved.get(i).filter(|m| worker::FanMode::decode(m).is_some()).cloned().unwrap_or_else(|| "auto".into());
            let _ = helper::rpc(&format!("set {i} {m}"));
        }
        let _ = smc;
    };
    let start = Instant::now();
    let mut ok = vec![false; n];
    // a fan passes once it has stayed in manual mode near the target for 3 s in a row
    let mut held = vec![0u32; n];
    let mut last = vec![(false, 0.0f64); n];
    let mut last_print = Instant::now() - Duration::from_secs(10);
    while start.elapsed() < Duration::from_secs(60) && !ok.iter().all(|x| *x) {
        for i in 0..n {
            let f = fans::read_fan(&mut smc, i, n);
            let close = (f.actual - targets[i]).abs() <= (0.1 * targets[i]).max(150.0);
            held[i] = if f.manual && close { held[i] + 1 } else { 0 };
            ok[i] = held[i] >= 6; // 6 readings, 0.5 s apart
            last[i] = (f.manual, f.actual);
            if last_print.elapsed() >= Duration::from_secs(5) {
                println!(
                    "  {:>2}s fan {i}: {} actual {:.0} rpm (target {:.0}){}",
                    start.elapsed().as_secs(),
                    if f.manual { "MANUAL" } else { "macOS " },
                    f.actual,
                    targets[i],
                    if ok[i] { "  ok" } else { "" }
                );
            }
        }
        if last_print.elapsed() >= Duration::from_secs(5) {
            if let Some(e) = helper::rpc("get").ok().and_then(|r| r.split('\t').nth(1).map(str::to_string)).filter(|e| !e.is_empty()) {
                println!("       helper reports: {e}");
            }
            last_print = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    for i in 0..n {
        println!(
            "fan {i}: final reading {} at {:.0} rpm (target {:.0}) -> {}",
            if last[i].0 { "MANUAL" } else { "macOS" },
            last[i].1,
            targets[i],
            if ok[i] { "held for 3 s" } else { "NOT under control" }
        );
    }
    restore(&mut smc);
    println!("Restored the previous fan modes.");
    let failed: Vec<usize> = (0..n).filter(|i| !ok[*i]).collect();
    if failed.is_empty() {
        println!("PASS: the helper took control of all {n} fan(s) and held them at the set speed ({}s).", start.elapsed().as_secs());
        Ok(())
    } else {
        Err(format!("FAIL: fan(s) {failed:?} never came under the helper's control within 60 s"))
    }
}
