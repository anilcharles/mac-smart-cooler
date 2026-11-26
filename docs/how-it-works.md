# How it works

```
 Mac Smart Cooler.app (runs as you)          background helper (runs as root)
 ┌──────────────────────────────┐            ┌──────────────────────────────┐
 │ window + menu bar icon       │  "get" /   │ worker loop, once a second:  │
 │ shows sensors, fans, status  │  "set …"   │  read sensors, fans, load    │
 │ sends mode changes ──────────┼──────────► │  run Smart Cooling           │
 │                              │ Unix socket│  write fan speeds to the SMC │
 └──────────────────────────────┘            └──────────────────────────────┘
```

- **Reading needs no privileges**, so the app reads sensors and fans itself.
- **Processes tab.** The app runs `/bin/ps` (installed setuid root by macOS, so it lists every
  account's processes) for each process's CPU time, and reads per-process GPU time from the
  GPU's IORegistry entries. It samples every 10 s, or every 2 s while the tab is on screen
  (about 2% of one core while it is), and works out usage from the change between samples.
  The helper is not involved and does not change.
- **Writing fan speeds needs root.** "Enable fan control" installs a launchd daemon
  (`local.macsmartcooler.helper`) that runs the same binary as root. The app sends it commands
  over `/var/run/macsmartcooler.sock`, so you only enter a password once.
- The helper saves the chosen preset in `/Library/Application Support/MacSmartCooler/settings.txt`
  (root-owned; only root writes it) and re-applies it at boot, so cooling continues after a
  restart even before you open the app. Without the helper, the app keeps its settings in
  your own `~/Library/Application Support/MacSmartCooler`.
- **Security:**
  - Any account can read the helper's status, but only root or the person logged in at
    the screen can change fans or settings. Callers are identified before they take a
    connection slot, and trusted callers have their own pool, so other accounts can't
    crowd them out. Every value is validated before it reaches the fan controller, and
    each request is limited in size and time.
  - Root reads settings only from its own root-owned file. Every settings read refuses
    symlinks, special files and files others can write, and stops at 4 KiB.
  - Approving the install prompt doesn't run the app from where it sits: root first copies
    the app into a fresh root-only folder, checks that the copy is signed by the team ID
    built into the release (`make-dmg.sh` sets it from your signing certificate), and only
    then runs that copy. Builds without a signing team skip the signature check.
  - The app asks for your password on its own only when the helper answers with a different
    version. A helper that isn't answering shows a **Repair helper** button instead.
  - **Known open items.** A source-only security audit (November 2026) found no confirmed
    vulnerabilities. It did leave these leads open; each needs a test on a spare Mac to
    confirm or rule out:
    - *Helper install, staging file.* While installing, root copies the helper to
      `/Library/PrivilegedHelperTools/local.macsmartcooler.helper.new`. That copy keeps the
      app's owner (you) until it is changed to root a moment later. A program running as you
      could add a write permission (ACL) to the file in that moment, and the permission would
      stay on the installed root helper. This applies to signed builds too.
    - *Helper install, unsigned builds.* With no signing team (plain `cargo build`, or
      `make-dmg.sh` that only prints *no signing team*), root runs whatever is in the app
      bundle when you approve the prompt. Ship only builds that printed `Team ID: …`.
    - *Helper install, launchd plist.* On a first install the plist is created with the
      calling app's umask and only then set to `0644`.
    - *Window run with `sudo`.* Appearance and zoom are saved to `ui.txt` under `$HOME`,
      following symlinks. If `sudo` keeps your `HOME`, root writes there. Use the background
      helper rather than running the window with `sudo`.
    - *Helper socket.* The helper sets the socket's mode by path in `/var/run`, which the
      `daemon` group can write. This matters only if a non-root process runs with that group.
- The helper's version is a hash of the fan-logic source files. When an app update changes
  that logic, the app notices the installed helper is outdated and asks to replace it.
- **If the helper crashes or is killed:** when launchd restarts it, it first hands any fan
  still held by the previous run back to macOS (and clears the `Ftst` unlock), then restores
  each fan's exact mode, Custom included. If its fan-control thread fails, it hands the fans
  back and exits so launchd starts a fresh one, rather than leaving fans at a fixed speed.
- **Apple Default** hands every fan back to macOS's own control. To remove the helper
  entirely, use **Preferences → Remove background helper**.

## Talking to the SMC

The SMC is reached through IOKit's `AppleSMC` service using four-character keys:

- `FNum` (number of fans), `F<n>Ac` (actual rpm), `F<n>Mn` / `F<n>Mx` (min/max rpm),
  `F<n>Tg` (target rpm);
- manual control via `F<n>Md` (plus `Ftst` on Apple Silicon) or `FS! ` on Intel;
- temperatures from keys such as `Tp0*` (CPU cores), `Tg0*` (GPU cores), `TaLP`/`TaRF`
  (case airflow) and `Ts0P`/`Ts1P` (palm rest).
