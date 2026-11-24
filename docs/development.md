# Development

## Build

    cargo test --release         # logic tests, incl. fan takeover on simulated Macs
    macsmartcooler selftest      # real hardware: the installed helper must take and hold the fans
    ./scripts/make-dmg.sh        # builds dist/Mac Smart Cooler.app and dist/MacSmartCooler.dmg

The tests replay situations measured on a real M1 Pro (app launches, noisy sensors, a game
launcher, hours of Blender) and check that Smart Cooling stays quiet or cools as expected.

### Release runner

Releases are built on a self-hosted macOS runner. It signs with the identity in the login
keychain of the account it runs as. When installed with `./svc.sh install`, the runner's
LaunchAgent has `SessionCreate` set to true. launchd then starts the runner in a new security
session where the login keychain is locked, so `codesign` fails with `errSecInternalComponent`.
Turn it off once (and again after any `svc.sh install`):

```sh
P=~/Library/LaunchAgents/actions.runner.<owner>-mac-smart-cooler.<host>.plist
plutil -replace SessionCreate -bool false "$P"
launchctl bootout gui/$(id -u) "$P"; launchctl bootstrap gui/$(id -u) "$P"
```

The runner then signs only while you're logged in with the login keychain unlocked.
`make-dmg.sh` checks this before building, and in CI it refuses to build without a signing team.

## Command line

The app's binary also works from the terminal (`/Applications/Mac Smart Cooler.app/Contents/MacOS/macsmartcooler`):

    macsmartcooler                   open the window
    macsmartcooler list              print fans and every temperature sensor
    macsmartcooler watch             print Smart Cooling's decision every second (changes nothing)
    sudo macsmartcooler set 0 3000   fan 0 to a fixed 3000 rpm
    sudo macsmartcooler auto [fan]   give one fan (or all) back to macOS
    sudo macsmartcooler smart        run Smart Cooling in the terminal until Ctrl-C
    macsmartcooler install-helper    install the background helper
    macsmartcooler uninstall-helper  remove it

`watch` is the easiest way to see why the fans are (or aren't) running.

To control the fans from the window, install the background helper rather than opening the
window with `sudo`. See *Known open items* in [How it works](how-it-works.md).

## Source layout

| File | Role |
|---|---|
| `src/smc.rs` | Raw AppleSMC client: read, write and list keys, decode value types |
| `src/fans.rs` | Fan info and switching fans between manual and automatic |
| `src/sensors.rs` | Finds temperature keys and gives them readable names |
| `src/load.rs` | CPU and GPU utilisation |
| `src/worker.rs` | The once-a-second control loop and Smart Cooling (`SmartCtl`) |
| `src/helper.rs` | Background helper: install/remove, socket server, restore preset at boot |
| `src/auth.rs` | macOS admin password prompt |
| `src/settings.rs` | Saved preset and preferences |
| `src/state.rs` | App-wide state: the worker, the menu bar item, settings, fan history |
| `src/ui.rs` | The window (GPUI): status card, modes, fans, sensors, dialogs |
| `src/widgets.rs`, `src/theme.rs` | Building blocks and colours (light/dark, zoom) |
| `src/charts.rs`, `src/elided.rs` | Ring gauge, rpm history, one-line text with "…" |
| `src/ui_prefs.rs` | Appearance and zoom (window only, not shared with the helper) |
| `src/tray.rs` | Menu bar icon and menu |
| `src/main.rs` | Command-line entry points, app menus and shortcuts |

Debug builds can capture every screen as PNGs from made-up data:
`scripts/screenshots.sh [dir]` (light and dark, 85%, 100% and 150% zoom).
