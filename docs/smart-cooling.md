# Modes and Smart Cooling

## Modes

The top of the window (and the menu bar icon) offers three presets:

| Mode | What the fans do |
|---|---|
| **Apple Default** | macOS controls the fans, exactly as if the app weren't installed. |
| **Smart Cooling** | macOS keeps the fans until heat builds up, then Mac Smart Cooler takes over and cools until the heat is gone (see below). |
| **Full Blast** | Every fan at its maximum speed. |

Each fan card also has its own **Auto | Smart | Custom** switch, so fans can run different
modes. **Custom** offers:

- **Fixed speed:** a constant rpm between the fan's minimum and maximum.
- **Follow a sensor:** a straight-line ramp from minimum speed at a start temperature to full
  speed at a top temperature, following any sensor you pick.

When fans run different modes, the top of the window shows **Custom**.

## How Smart Cooling works

Apple Silicon is designed to run hot. A chip at 80 °C is normal, so chip temperature alone
is a poor reason to spin the fans. Smart Cooling instead watches what you actually feel and
what builds up over time. Once a second it reads:

- **Chip:** the hottest CPU/GPU core.
- **Case air:** the airflow sensors in the back of the case, by the vents.
- **Palm rest:** the surface temperature under your wrists.
- **Battery:** its temperature, and whether it's charging. Charging heats the battery even
  when the Mac is idle, and sustained heat above ~40 °C wears lithium cells faster.
- **Load:** CPU and GPU utilisation.

### Filtering out noise

Core temperatures jump by several degrees from one second to the next, and launching an app
spikes them for a moment. Before any decision is made:

- the chip reading goes through a 9-second median filter (spikes shorter than ~4 s vanish),
  then a ~30-second average;
- load is averaged over ~30 seconds, so a burst of work doesn't count as "busy" (busy means
  over 50% sustained);
- case air is averaged over ~15 seconds.

### When the fans take over

macOS keeps the fans until one of these holds:

| Condition | Must last |
|---|---|
| Busy **and** case air ≥ 45 °C, or busy **and** palm rest ≥ 35 °C | 20 s |
| Case air ≥ 52 °C or palm rest ≥ 38 °C, busy or not | 20 s |
| Charging **and** battery ≥ 38 °C, or battery ≥ 41 °C at any time | 20 s |
| Busy **and** chip past halfway between "cooling starts" and "full speed" (74 °C by default) | 2½ min |
| Chip at the "full speed" temperature (88 °C by default), busy or not | 45 s |

So opening apps, compiling for a minute or a game launcher updating won't spin the fans up,
but a long render will.

### How fast the fans spin

While cooling, the speed is the highest of three demands:

- **Case air:** minimum speed at 42 °C, full speed at 64 °C. This counts the most.
- **Palm rest:** minimum speed at 35 °C, full speed at 42 °C.
- **Battery:** minimum speed at 37 °C, full speed at 47 °C.
- **Chip:** counts at half weight up to the "full speed" temperature (the trend over the last
  minute is added, so a chip that's climbing gets a head start). Above it the chip alone
  ramps the fans to full speed over the next 6 °C.

**Heat soak.** That curve alone settles wherever heat in equals heat out. During a long
Blender render that left the case air stuck around 51 °C at about 3,000 rpm while the
bottom of the case kept getting hotter. So on top of the curve, Smart Cooling keeps adding
speed for as long as the case air stays above **45 °C** (or the palm rest above 36 °C, or the
battery above 39 °C). The
further over, the faster it climbs: 5 °C over adds a step about every 50 seconds. If the heat
never drops, the fans reach full speed. Once the case is more than 1 °C under target, the
extra speed fades slowly (about a step every 2 minutes).

**Steady, not hunting.** The speed moves in 8 fixed steps between the fan's minimum and
maximum. It steps up as soon as more cooling is needed, but only steps down after 30 seconds
of headroom. The actual rpm then ramps toward the step: up to 300 rpm/s when speeding up,
80 rpm/s when slowing down, so changes are quick when it matters and quiet otherwise.

### Handing back to macOS

The fans go back to macOS after **45 seconds of calm**: palm rest under 34 °C, case air under
42 °C, battery under 37 °C, chip under 85 °C, and either the chip below 60 °C or the work has stopped and the
chip is no longer warming.

The status card shows what Smart Cooling is doing and why, for example
*"Quiet: no sustained heat, fans left to macOS"* or
*"Cooling: heat is building up, pushing harder"*.

**Preferences** let you move the two chip temperatures ("Cooling starts at", default 60 °C,
and "Full speed at", default 88 °C). Case air and palm rest thresholds are fixed.

## Supported Macs

- **Apple silicon M1 to M6** (base, Pro, Max, Ultra) and **Intel** Macs with fans. Fanless
  Macs (MacBook Air) can monitor temperatures only.
- Apple reuses sensor keys with different meanings on each chip generation, so core sensors
  are named from a per-generation table, based on the one maintained by the open-source
  [Stats](https://github.com/exelban/stats) app. On a chip newer than the table, its CPU/GPU
  sensors are still found by key family and shown as "Chip Sensor …", so Smart Cooling
  always has a chip temperature.
- "Hottest CPU/GPU" uses every CPU/GPU-family sensor (`Tp`/`Te`/`Tf`/`Tg`), not only the
  labelled cores. The labels differ between sources (Stats and
  [iSMC](https://github.com/dkorunic/iSMC) disagree on M3), and on an M1 Max the labelled
  cores read about 10 °C below the SMC's own "CPU die max" (`TCMz`), which this matches.
- `Ts0P`/`Ts1P` are used as palm-rest temperature, following the
  [Asahi Linux SMC notes](https://asahilinux.org/docs/hw/soc/smc/); iSMC labels them SSD
  controller sensors instead, so treat that reading with some caution.
- Manual fan control follows each generation's method: M5 and later switch fans directly;
  M1-M4 first unlock fan control from macOS (`Ftst`), which can take a few seconds. Some
  Macs use a lowercase fan-mode key (`F0md`); both are handled.
- Smart Cooling's case-air and palm-rest thresholds were tuned on a 16-inch M1 Max. Other
  models may run warmer or cooler; `macsmartcooler watch` shows what it decides and why.
- Only the 16-inch M1 Max has been tested on real hardware so far.
