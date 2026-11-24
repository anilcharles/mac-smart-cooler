//! Derives the background helper's version from the sources it runs, so an app update
//! that changes fan logic always replaces the installed helper.
fn main() {
    let files = [
        "src/worker.rs",
        "src/helper.rs",
        "src/fans.rs",
        "src/smc.rs",
        "src/sensors.rs",
        "src/load.rs",
        "src/settings.rs",
    ];
    println!("cargo:rerun-if-env-changed=MACSMARTCOOLER_TEAM_ID");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a
    for f in files {
        println!("cargo:rerun-if-changed={f}");
        for b in std::fs::read(f).unwrap_or_default() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    println!("cargo:rustc-env=MACSMARTCOOLER_HELPER_HASH={h:016x}");
}
