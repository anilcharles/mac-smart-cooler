//! Background helper (launchd daemon running as root).
//!
//! Installed once (one admin prompt). Afterwards the normal, unprivileged app sends
//! fan commands to it over a Unix socket, so it never needs a password again, and
//! the chosen preset keeps running when the app is closed and after reboots.

use crate::settings;
use crate::smc::Res;
use crate::worker::{Cmd, FanMode, Worker};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

pub const LABEL: &str = "local.macsmartcooler.helper";
pub const SOCK: &str = "/var/run/macsmartcooler.sock";

/// Socket path (overridable for testing with MACSMARTCOOLER_SOCK).
pub(crate) fn sock() -> String {
    std::env::var("MACSMARTCOOLER_SOCK").unwrap_or_else(|_| SOCK.to_string())
}
const BIN: &str = "/Library/PrivilegedHelperTools/local.macsmartcooler.helper";
const PLIST: &str = "/Library/LaunchDaemons/local.macsmartcooler.helper.plist";
// the helper from when the app was called MacBreeze: removed on install so two helpers never fight
const OLD_LABEL: &str = "local.macbreeze.helper";
const OLD_FILES: [&str; 3] = [
    "/Library/LaunchDaemons/local.macbreeze.helper.plist",
    "/Library/PrivilegedHelperTools/local.macbreeze.helper",
    "/var/run/macbreeze.sock",
];

// ---------------------------------------------------------------- client side

/// Send one command line, get one reply line.
pub fn rpc(line: &str) -> Res<String> {
    let mut s = UnixStream::connect(sock()).map_err(|e| format!("helper not reachable: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(8))).ok();
    s.set_write_timeout(Some(Duration::from_secs(2))).ok();
    writeln!(s, "{line}").map_err(|e| e.to_string())?;
    let mut out = String::new();
    BufReader::new(s)
        .read_line(&mut out)
        .map_err(|e| e.to_string())?;
    Ok(out.trim_end().to_string())
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    NotInstalled,
    Outdated,
    /// installed but not answering (stopped, crashed, or busy): never prompts on its own
    Unreachable,
    Installed,
}

/// Hash of the helper-side sources (see build.rs): any change to fan logic changes it,
/// so the app always asks (one password prompt) to replace an outdated helper.
pub const HELPER_VERSION: &str = env!("MACSMARTCOOLER_HELPER_HASH");

/// Installed = the helper is running and is this exact build. Outdated = it answered with
/// another version. Unreachable = it didn't answer: that alone never triggers a password
/// prompt, so a stalled or busy socket can't be used to pop up admin dialogs.
pub fn status() -> Status {
    if !std::path::Path::new(PLIST).exists() {
        return Status::NotInstalled;
    }
    match rpc("version") {
        Ok(v) if v == HELPER_VERSION => Status::Installed,
        Ok(v) if !v.is_empty() => Status::Outdated,
        _ => Status::Unreachable,
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Run a shell command as root through the AppleScript admin prompt. The command and
/// prompt go in as osascript arguments, so nothing is written to disk and nothing needs
/// AppleScript escaping.
fn run_as_admin(command: &str, prompt: &str) -> Res<bool> {
    let status = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "do shell script (item 1 of argv) with prompt (item 2 of argv) with administrator privileges",
            "-e",
            "end run",
            command,
            prompt,
        ])
        .status()
        .map_err(|e| e.to_string())?;
    Ok(status.success())
}

/// Team ID the release build was signed with (set by scripts/make-dmg.sh). When present,
/// the privileged step refuses to run an app copy that isn't signed by this team.
const TEAM_ID: Option<&str> = option_env!("MACSMARTCOOLER_TEAM_ID");

fn team_id() -> Option<&'static str> {
    TEAM_ID.filter(|t| t.len() == 10 && t.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
}

/// The script root runs for install/uninstall. Approving the prompt must not mean "run
/// whatever file sits at a path this user can write", so root first copies the app into
/// a fresh root-only folder, checks the copy's signature (when the build is team-signed),
/// and only then runs that copy. $1 = app bundle (or bare binary), $2 = install|uninstall.
fn privileged_script() -> String {
    let verify = match team_id() {
        Some(t) => format!(
            "/usr/bin/codesign --verify --strict --deep -R='anchor apple generic and certificate leaf[subject.OU] = \"{t}\"' \"$v\" || {{ echo 'Mac Smart Cooler: app signature check failed' >&2; exit 3; }}\n"
        ),
        None => String::new(),
    };
    format!(
        r#"set -eu
d=$(/usr/bin/mktemp -d /private/var/tmp/macsmartcooler.XXXXXXXX)
trap '/bin/rm -rf "$d"' EXIT
if [ -d "$1" ]; then
  /usr/bin/ditto "$1" "$d/app.app"
  v="$d/app.app"
  exe="$d/app.app/Contents/MacOS/macsmartcooler"
else
  /bin/cp -X "$1" "$d/macsmartcooler"
  v="$d/macsmartcooler"
  exe="$v"
fi
{verify}case "$2" in
  install) "$exe" __install-root "$exe" ;;
  uninstall) "$exe" __uninstall-root ;;
  *) exit 2 ;;
esac
"#
    )
}

/// The app bundle this binary lives in (…/X.app), or the bare binary for dev builds.
fn app_path() -> Res<String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bundle = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .filter(|b| b.extension().is_some_and(|e| e == "app"));
    Ok(bundle.unwrap_or(&exe).to_string_lossy().into_owned())
}

/// Run the privileged install/uninstall step through the admin prompt.
fn run_privileged(action: &str, prompt: &str) -> Res<bool> {
    let app = app_path()?;
    let script = privileged_script();
    match crate::auth::run_as_root("/bin/sh", &["-c", &script, "sh", &app, action], prompt, app_icon().as_deref()) {
        Ok(done) => Ok(done),
        // only when Authorization Services can't start the tool at all; a failed or
        // cancelled run must not lead to a second prompt
        Err(e) if e.starts_with(crate::auth::CANNOT_START) => {
            let cmd = format!("/bin/sh -c {} sh {} {}", shell_quote(&script), shell_quote(&app), action);
            run_as_admin(&cmd, prompt)
        }
        Err(e) => Err(e),
    }
}

fn app_icon() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let icns = exe.parent()?.parent()?.join("Resources/AppIcon.icns");
    icns.exists().then(|| icns.to_string_lossy().into_owned())
}

fn plist_text() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key><array><string>{BIN}</string><string>daemon</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
</dict>
</plist>
"#
    )
}

/// Install/update the helper. Shows the system admin prompt (Touch ID or password) once.
pub fn install() -> Res<bool> {
    run_privileged("install", "Mac Smart Cooler wants to install its fan helper. You only need to do this once.")
}

pub fn uninstall() -> Res<bool> {
    run_privileged("uninstall", "Mac Smart Cooler wants to remove its fan helper.")
}

extern "C" {
    fn setuid(uid: u32) -> i32;
}

fn launchctl(args: &[&str]) -> bool {
    std::process::Command::new("/bin/launchctl")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Runs as root (from the admin prompt): copy the binary, write the launchd job, start it.
pub fn install_as_root(src: &str) -> Res<()> {
    use std::os::unix::fs::chown;
    unsafe {
        setuid(0); // make real uid root too, so launchctl treats us as root
    }
    if !crate::smc::is_root() {
        return Err("must run as root".into());
    }
    launchctl(&["bootout", &format!("system/{OLD_LABEL}")]);
    for p in OLD_FILES {
        let _ = std::fs::remove_file(p);
    }
    let target = format!("system/{LABEL}");
    launchctl(&["bootout", &target]);
    std::fs::create_dir_all("/Library/PrivilegedHelperTools").map_err(|e| e.to_string())?;
    let tmp = format!("{BIN}.new");
    std::fs::copy(src, &tmp).map_err(|e| format!("copy helper: {e}"))?;
    // The app's signature is tied to its bundle; a copied-out binary must be re-signed
    // or macOS kills it at launch (OS_REASON_EXEC).
    let signed = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", &tmp])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !signed {
        return Err("could not sign the helper".into());
    }
    chown(&tmp, Some(0), Some(0)).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, BIN).map_err(|e| e.to_string())?;
    std::fs::write(PLIST, plist_text()).map_err(|e| format!("write {PLIST}: {e}"))?;
    chown(PLIST, Some(0), Some(0)).map_err(|e| e.to_string())?;
    std::fs::set_permissions(PLIST, std::fs::Permissions::from_mode(0o644))
        .map_err(|e| e.to_string())?;
    for _ in 0..3 {
        if launchctl(&["bootstrap", "system", PLIST]) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err("launchctl could not start the helper".into())
}

pub fn uninstall_as_root() -> Res<()> {
    unsafe {
        setuid(0);
    }
    launchctl(&["bootout", &format!("system/{LABEL}")]);
    launchctl(&["bootout", &format!("system/{OLD_LABEL}")]);
    for p in [PLIST, BIN, SOCK].into_iter().chain(OLD_FILES) {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

// ---------------------------------------------------------------- daemon side

fn sanitize(s: &str) -> String {
    s.replace(['\n', '\t', '\r'], " ")
}

/// Entry point for "macsmartcooler daemon" (run by launchd as root).
pub fn serve() -> Res<()> {
    if !crate::smc::is_root() && std::env::var_os("MACSMARTCOOLER_SOCK").is_none() {
        return Err("the helper must run as root".into());
    }
    let sock = sock();
    // A previous helper that was killed or crashed ran no exit handler, so fans may still be
    // held at its last speed (macOS's thermal control locked out). Start from a clean state.
    if crate::smc::is_root() {
        if let Ok(mut smc) = crate::smc::Smc::open() {
            crate::fans::release_stale(&mut smc);
        }
    }
    let worker = std::sync::Arc::new(Worker::spawn(|| {}));

    // Start answering the app immediately (before sensor discovery finishes).
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).map_err(|e| format!("bind {sock}: {e}"))?;
    // anyone may connect (status is shown to every account); changes are checked per caller
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o666));

    // Re-apply the preset the user last chose, so cooling survives reboots.
    let w2 = worker.clone();
    std::thread::spawn(move || restore_saved_preset(&w2));

    // Callers are identified before they take a slot, and trusted callers (root, the
    // person at the screen) have their own pool, so other accounts can't fill it.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let trusted_busy = std::sync::Arc::new(AtomicUsize::new(0));
    let other_busy = std::sync::Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let trusted = peer_trusted(&stream);
        let (pool, cap) = if trusted {
            (trusted_busy.clone(), MAX_TRUSTED)
        } else {
            (other_busy.clone(), MAX_OTHER)
        };
        if pool.load(Ordering::SeqCst) >= cap {
            continue; // dropped: that pool is full
        }
        pool.fetch_add(1, Ordering::SeqCst);
        let worker = worker.clone();
        std::thread::spawn(move || {
            // a panic while serving one client must not leak its slot
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve_one(&worker, stream, trusted)));
            pool.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

const MAX_TRUSTED: usize = 16;
const MAX_OTHER: usize = 8;
const MAX_LINE: usize = 512;
const CLIENT_DEADLINE: Duration = Duration::from_secs(2);

fn serve_one(worker: &Worker, mut stream: UnixStream, trusted: bool) {
    let Some(line) = read_line_bounded(&mut stream) else {
        return;
    };
    let reply = handle(worker, line.trim(), trusted);
    stream.set_write_timeout(Some(CLIENT_DEADLINE)).ok();
    let _ = writeln!(stream, "{reply}");
}

/// Read one command line: at most MAX_LINE bytes within CLIENT_DEADLINE in total.
fn read_line_bounded(stream: &mut UnixStream) -> Option<String> {
    use std::io::Read;
    let deadline = std::time::Instant::now() + CLIENT_DEADLINE;
    let mut buf = Vec::with_capacity(64);
    let mut chunk = [0u8; 128];
    while !buf.contains(&b'\n') {
        let left = deadline.checked_duration_since(std::time::Instant::now())?;
        stream.set_read_timeout(Some(left.max(Duration::from_millis(1)))).ok()?;
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_LINE {
            return None;
        }
    }
    let end = buf.iter().position(|&b| b == b'\n').unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec()).ok()
}

extern "C" {
    fn getpeereid(fd: i32, euid: *mut u32, egid: *mut u32) -> i32;
    fn geteuid() -> u32;
}

/// Changes are accepted only from root, the helper's own user (tests), or whoever is
/// logged in at the screen. Other accounts can still read the status.
fn peer_trusted(stream: &UnixStream) -> bool {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::io::AsRawFd;
    let (mut uid, mut gid) = (u32::MAX, u32::MAX);
    if unsafe { getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return false;
    }
    let console = std::fs::metadata("/dev/console").map(|m| m.uid()).ok();
    uid == 0 || uid == unsafe { geteuid() } || Some(uid) == console
}

fn restore_saved_preset(worker: &Worker) {
    let s = settings::load();
    for _ in 0..100 {
        if worker.snapshot.lock().unwrap_or_else(|e| e.into_inner()).ready {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let fans = worker.snapshot.lock().unwrap_or_else(|e| e.into_inner()).fans.clone();
    for (i, mode) in modes_to_restore(&s, &fans) {
        worker.send(Cmd::SetMode(i, mode));
    }
}

/// What to re-apply at start: each fan's exact saved mode when it matches this Mac's fans,
/// otherwise the saved preset (Smart Cooling or Full Blast for every fan).
fn modes_to_restore(s: &settings::Settings, fans: &[crate::fans::FanInfo]) -> Vec<(usize, FanMode)> {
    let saved: Vec<FanMode> = s.fan_modes.split('|').filter_map(FanMode::decode).collect();
    if !s.fan_modes.is_empty() && saved.len() == fans.len() {
        return fans
            .iter()
            .zip(saved)
            .filter(|(_, m)| *m != FanMode::Auto)
            .map(|(f, m)| (f.index, m))
            .collect();
    }
    fans.iter()
        .filter_map(|f| match s.preset.as_str() {
            "smart" => Some((f.index, FanMode::Smart { lo: s.smart_lo, hi: s.smart_hi })),
            "full" => Some((f.index, FanMode::Constant(f.max))),
            _ => None,
        })
        .collect()
}

/// Record every fan's current mode so a restarted helper brings back exactly this state.
fn remember_modes(worker: &Worker) {
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let modes = worker.snapshot.lock().unwrap_or_else(|e| e.into_inner()).modes.clone();
    if modes.is_empty() {
        return;
    }
    let enc: Vec<String> = modes.iter().map(FanMode::encode).collect();
    let enc = enc.join("|");
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if *last == enc {
        return;
    }
    let mut s = settings::load();
    s.fan_modes = enc.clone();
    if settings::save_system(&s).is_ok() {
        *last = enc;
    }
}

fn handle(worker: &Worker, line: &str, trusted: bool) -> String {
    let mut parts = line.splitn(3, ' ');
    let cmd = parts.next();
    if matches!(cmd, Some("set" | "save")) && !trusted {
        return "not allowed".into();
    }
    if let (Some("save"), Some(rest)) = (cmd, line.strip_prefix("save ")) {
        // the helper owns the per-fan modes record; keep it, whatever the app sends
        let mut new = settings::parse(rest);
        new.fan_modes = settings::load().fan_modes;
        return match settings::save_system(&new) {
            Ok(()) => "ok".into(),
            Err(e) => sanitize(&e),
        };
    }
    match (cmd, parts.next(), parts.next()) {
        (Some("ping"), _, _) => "pong".into(),
        (Some("version"), _, _) => HELPER_VERSION.into(),
        (Some("get"), _, _) => {
            let s = worker
                .snapshot
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let modes: Vec<String> = s.modes.iter().map(FanMode::encode).collect();
            format!(
                "{}\t{}\t{}",
                modes.join("|"),
                sanitize(&s.error.unwrap_or_default()),
                sanitize(&s.activity.smart.unwrap_or_default())
            )
        }
        (Some("set"), Some(i), Some(m)) => match (i.parse::<usize>(), FanMode::decode(m)) {
            (Ok(i), Some(mode)) => {
                if !worker.send(Cmd::SetMode(i, mode.clone())) {
                    return "error: fan control has stopped".into();
                }
                // reply once the worker has taken it, so the app's next "get" is already current
                for _ in 0..50 {
                    if worker
                        .snapshot
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .modes
                        .get(i)
                        == Some(&mode)
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                remember_modes(worker);
                "ok".into()
            }
            _ => "bad command".into(),
        },
        _ => "bad command".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_reads_are_bounded() {
        // normal command
        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.write_all(b"get\n").unwrap();
        assert_eq!(read_line_bounded(&mut b).as_deref(), Some("get"));
        // an endless line is cut off instead of growing the root helper's memory
        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.write_all(&vec![b'x'; 4096]).unwrap();
        assert_eq!(read_line_bounded(&mut b), None);
        // a client that never finishes its line is dropped after the deadline
        let (_a, mut b) = UnixStream::pair().unwrap();
        let t = std::time::Instant::now();
        assert_eq!(read_line_bounded(&mut b), None);
        assert!(t.elapsed() < CLIENT_DEADLINE + Duration::from_millis(500));
        // the caller's identity is readable (same user here, so trusted)
        let (a, _b) = UnixStream::pair().unwrap();
        assert!(peer_trusted(&a));
    }

    fn fan(i: usize) -> crate::fans::FanInfo {
        crate::fans::FanInfo { index: i, name: String::new(), min: 1500.0, max: 5000.0 + i as f64, actual: 0.0, target: 0.0, manual: false }
    }

    #[test]
    fn restart_restores_each_fans_exact_mode() {
        let fans = [fan(0), fan(1)];
        // custom: fan 0 fixed, fan 1 follows a sensor; the preset says smart but the record wins
        let s = settings::Settings {
            preset: "smart".into(),
            fan_modes: "const,3000|sensor,max:hot,45,80".into(),
            ..Default::default()
        };
        let m = modes_to_restore(&s, &fans);
        assert_eq!(m, vec![
            (0, FanMode::Constant(3000.0)),
            (1, FanMode::Sensor { key: "max:hot".into(), lo: 45.0, hi: 80.0 }),
        ]);
        // fans left with macOS stay with macOS
        let s = settings::Settings { fan_modes: "auto|const,4000".into(), ..Default::default() };
        assert_eq!(modes_to_restore(&s, &fans), vec![(1, FanMode::Constant(4000.0))]);
        // a record for a different number of fans is ignored in favour of the preset
        let s = settings::Settings { preset: "full".into(), fan_modes: "const,3000".into(), ..Default::default() };
        assert_eq!(modes_to_restore(&s, &fans), vec![(0, FanMode::Constant(5000.0)), (1, FanMode::Constant(5001.0))]);
        // nothing recorded, Apple Default: nothing to apply
        assert!(modes_to_restore(&settings::Settings::default(), &fans).is_empty());
    }

    #[test]
    fn privileged_script_is_valid_and_verifies_when_team_signed() {
        let script = privileged_script();
        let ok = std::process::Command::new("/bin/sh")
            .args(["-n", "-c", &script])
            .status()
            .unwrap()
            .success();
        assert!(ok, "install script has a shell syntax error:\n{script}");
        // root runs a copy in its own folder, never the caller's path
        assert!(script.contains("mktemp -d /private/var/tmp/"));
        assert!(script.contains("\"$exe\" __install-root \"$exe\""));
        assert_eq!(script.contains("codesign --verify"), team_id().is_some());
    }
}
