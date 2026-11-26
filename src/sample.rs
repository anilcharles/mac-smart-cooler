//! Made-up fans and sensors for screenshots (debug builds, `MACSMARTCOOLER_SAMPLE=1`).
//! Nothing here reads or touches the hardware.

use crate::fans::FanInfo;
use crate::procs::{FanEvent, Latest, Owner, Usage};
use crate::sensors::{Reading, CPU_AVG_KEY, GPU_AVG_KEY, HOT_KEY};
use crate::worker::{Activity, FanMode, Snapshot};
use std::collections::VecDeque;

fn reading(key: &str, name: &str, value: f64) -> Reading {
    Reading { key: key.into(), name: name.into(), value, named: true }
}

pub fn snapshot() -> Snapshot {
    let fans = vec![
        FanInfo { index: 0, name: "Left side".into(), min: 1200., max: 5700., actual: 2450., target: 2450., manual: true },
        FanInfo { index: 1, name: "Right side".into(), min: 1200., max: 6150., actual: 2580., target: 2580., manual: true },
    ];
    let mut sensors = vec![
        reading(HOT_KEY, "Hottest CPU/GPU", 71.4),
        reading(CPU_AVG_KEY, "CPU Core Average", 58.2),
        reading("Tp01", "CPU Performance Core 1", 66.9),
        reading("Tp05", "CPU Performance Core 2", 63.1),
        reading("Tp09", "CPU Performance Core 3", 60.4),
        reading("Tp0D", "CPU Performance Core 4", 57.8),
        reading("Te05", "CPU Efficiency Core 1", 46.3),
        reading("Te0L", "CPU Efficiency Core 2", 44.9),
        reading(GPU_AVG_KEY, "GPU Core Average", 49.6),
        reading("Tg05", "GPU Core 1", 52.0),
        reading("Tg0D", "GPU Core 2", 47.2),
        reading("TB0T", "Battery", 33.1),
        reading("TB1T", "Battery Gas Gauge", 32.4),
        reading("Ts0P", "Palm Rest", 31.6),
        reading("TaLP", "Airflow Left", 40.8),
        reading("TaRF", "Airflow Right", 41.9),
        reading("TH0x", "SSD", 44.0),
        reading("TW0P", "Airport Proximity", 43.5),
        reading("Ts0S", "Memory Proximity", 47.7),
    ];
    for (k, v) in [("Tp1h", 55.0), ("TPMP", 38.2), ("TVA0", 36.9)] {
        sensors.push(Reading { key: k.into(), name: format!("SMC {k}"), value: v, named: false });
    }
    Snapshot {
        activity: Activity {
            cpu: 0.38,
            gpu: Some(0.14),
            sustained: 0.31,
            chassis: Some(31.6),
            body: Some(41.4),
            trend: 0.8,
            smart: Some("Cooling: back of the case is warm".into()),
            battery: Some(33.1),
            charging: false,
        },
        ready: true,
        root: false,
        can_control: true,
        modes: vec![FanMode::Smart { lo: 60., hi: 88. }; 2],
        fans,
        sensors,
        error: None,
    }
}

/// Two minutes of history for a fan that spun up a minute ago.
pub fn history(fan: usize, now: f32) -> VecDeque<f32> {
    (0..120)
        .map(|i| {
            let t = i as f32;
            let ramp = ((t - 55.) / 20.).clamp(0., 1.);
            let wobble = ((t + fan as f32 * 7.) * 0.45).sin() * 40.;
            if ramp <= 0. { 0. } else { 1200. + (now - 1200.) * ramp + wobble }
        })
        .collect()
}

fn usage(name: &str, pids: &[i32], cpu: f64, gpu: f64, owner: Owner) -> Usage {
    Usage { name: name.into(), pids: pids.to_vec(), cpu, gpu, owner }
}

/// A made-up process table: a render in Blender, with the usual company.
pub fn procs() -> Latest {
    let table = vec![
        usage("Blender", &[4120], 3.82, 0.41, Owner::You),
        usage("WindowServer", &[194], 0.31, 0.12, Owner::System),
        usage("mds_stores", &[269], 0.22, 0., Owner::System),
        usage("Safari Web Content", &[2210, 2214, 2290], 0.14, 0.02, Owner::You),
        usage("kernel_task", &[0], 0.09, 0., Owner::System),
        usage("Code Helper (Renderer)", &[1295, 1301], 0.06, 0.004, Owner::You),
        usage("Music", &[880], 0.031, 0.001, Owner::You),
        usage("backupd", &[402], 0.027, 0., Owner::System),
        usage("Finder", &[641], 0.012, 0., Owner::You),
        usage("Mac Smart Cooler", &[5011], 0.008, 0.001, Owner::You),
        usage("bluetoothd", &[171], 0.006, 0., Owner::System),
        usage("coreaudiod", &[226], 0.004, 0., Owner::System),
    ];
    Latest { minute: table.clone(), table, gpu_known: true, span: 30., error: None }
}

/// Two made-up times the fans came on.
pub fn spins() -> VecDeque<FanEvent> {
    let now = std::time::Instant::now();
    let wall = std::time::SystemTime::now();
    let ago = |m: u64| std::time::Duration::from_secs(m * 60);
    VecDeque::from(vec![
        FanEvent {
            wall: wall - ago(6),
            started: now - ago(6),
            lasted: None,
            peak_rpm: 2580.,
            why: Some("back of the case is warm".into()),
            at_launch: false,
            culprits: vec![
                usage("Blender", &[4120], 3.64, 0.38, Owner::You),
                usage("WindowServer", &[194], 0.28, 0.10, Owner::System),
                usage("mds_stores", &[269], 0.19, 0., Owner::System),
            ],
        },
        FanEvent {
            wall: wall - ago(52),
            started: now - ago(52),
            lasted: Some(ago(3)),
            peak_rpm: 1650.,
            why: None,
            at_launch: false,
            culprits: vec![
                usage("mds_stores", &[269], 1.21, 0., Owner::System),
                usage("mdworker_shared", &[3301, 3302, 3307], 0.64, 0., Owner::System),
                usage("Safari Web Content", &[2210], 0.12, 0.03, Owner::You),
            ],
        },
    ])
}
