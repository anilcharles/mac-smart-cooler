//! CPU and GPU utilisation (no root needed).

use std::ffi::{c_char, c_void, CString};

type CFTypeRef = *const c_void;
const UTF8: u32 = 0x0800_0100;
const CF_NUMBER_SINT64: i32 = 4;
const HOST_CPU_LOAD_INFO: i32 = 3;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(alloc: CFTypeRef, s: *const c_char, enc: u32) -> CFTypeRef;
    fn CFDictionaryGetValue(d: CFTypeRef, k: CFTypeRef) -> CFTypeRef;
    fn CFNumberGetValue(n: CFTypeRef, t: i32, out: *mut c_void) -> bool;
    fn CFGetTypeID(x: CFTypeRef) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFRelease(x: CFTypeRef);
    fn CFDataGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(b: CFTypeRef) -> bool;
    fn CFDataGetLength(d: CFTypeRef) -> isize;
    fn CFDataGetBytePtr(d: CFTypeRef) -> *const u8;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main: u32, matching: *mut c_void, iter: *mut u32) -> i32;
    fn IOIteratorNext(iter: u32) -> u32;
    fn IOObjectRelease(o: u32) -> i32;
    fn IORegistryEntryFromPath(main: u32, path: *const c_char) -> u32;
    fn IORegistryEntryCreateCFProperty(
        entry: u32,
        key: CFTypeRef,
        alloc: CFTypeRef,
        opts: u32,
    ) -> CFTypeRef;
}

extern "C" {
    fn mach_host_self() -> u32;
    fn host_statistics(host: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
}

/// Whole-machine CPU busy fraction (0..1) between consecutive calls.
pub struct CpuSampler {
    host: u32,
    prev: Option<[u32; 4]>,
}

impl CpuSampler {
    pub fn new() -> Self {
        CpuSampler {
            host: unsafe { mach_host_self() },
            prev: None,
        }
    }

    pub fn sample(&mut self) -> Option<f64> {
        let mut info = [0i32; 4]; // user, system, idle, nice
        let mut count = 4u32;
        if unsafe { host_statistics(self.host, HOST_CPU_LOAD_INFO, info.as_mut_ptr(), &mut count) }
            != 0
        {
            return None;
        }
        let cur = info.map(|v| v as u32);
        let out = self.prev.map(|p| {
            let d = |i: usize| cur[i].wrapping_sub(p[i]) as f64;
            let busy = d(0) + d(1) + d(3);
            let total = busy + d(2);
            if total > 0.0 {
                busy / total
            } else {
                0.0
            }
        });
        self.prev = Some(cur);
        out
    }
}

/// GPU "Device Utilization %" from the IOAccelerator registry entry, as 0..1.
pub fn gpu_utilization() -> Option<f64> {
    unsafe {
        let class = CString::new("IOAccelerator").ok()?;
        let mut iter = 0u32;
        if IOServiceGetMatchingServices(0, IOServiceMatching(class.as_ptr()), &mut iter) != 0 {
            return None;
        }
        let stats_key = CString::new("PerformanceStatistics").ok()?;
        let util_key = CString::new("Device Utilization %").ok()?;
        let k_stats = CFStringCreateWithCString(std::ptr::null(), stats_key.as_ptr(), UTF8);
        let k_util = CFStringCreateWithCString(std::ptr::null(), util_key.as_ptr(), UTF8);
        let mut best: Option<f64> = None;
        loop {
            let entry = IOIteratorNext(iter);
            if entry == 0 {
                break;
            }
            let dict = IORegistryEntryCreateCFProperty(entry, k_stats, std::ptr::null(), 0);
            if !dict.is_null() {
                if CFGetTypeID(dict) == CFDictionaryGetTypeID() {
                    let n = CFDictionaryGetValue(dict, k_util);
                    let mut v: i64 = 0;
                    if !n.is_null()
                        && CFGetTypeID(n) == CFNumberGetTypeID()
                        && CFNumberGetValue(n, CF_NUMBER_SINT64, &mut v as *mut i64 as *mut c_void)
                    {
                        let f = (v as f64 / 100.0).clamp(0.0, 1.0);
                        best = Some(best.map_or(f, |b: f64| b.max(f)));
                    }
                }
                CFRelease(dict);
            }
            IOObjectRelease(entry);
        }
        IOObjectRelease(iter);
        CFRelease(k_stats);
        CFRelease(k_util);
        best
    }
}

/// Whether the battery is charging right now (None on Macs without a battery).
pub fn charging() -> Option<bool> {
    unsafe {
        let class = CString::new("AppleSmartBattery").ok()?;
        let mut iter = 0u32;
        if IOServiceGetMatchingServices(0, IOServiceMatching(class.as_ptr()), &mut iter) != 0 {
            return None;
        }
        let key_c = CString::new("IsCharging").ok()?;
        let key = CFStringCreateWithCString(std::ptr::null(), key_c.as_ptr(), UTF8);
        let mut out = None;
        loop {
            let entry = IOIteratorNext(iter);
            if entry == 0 {
                break;
            }
            if !key.is_null() && out.is_none() {
                let v = IORegistryEntryCreateCFProperty(entry, key, std::ptr::null(), 0);
                if !v.is_null() {
                    if CFGetTypeID(v) == CFBooleanGetTypeID() {
                        out = Some(CFBooleanGetValue(v));
                    }
                    CFRelease(v);
                }
            }
            IOObjectRelease(entry);
        }
        IOObjectRelease(iter);
        if !key.is_null() {
            CFRelease(key);
        }
        out
    }
}

/// The Mac's marketing name as macOS stores it, e.g. "MacBook Pro (16-inch, 2021)".
/// Apple silicon Macs keep it in the device tree; older Macs may not have it.
pub fn product_name() -> Option<String> {
    unsafe {
        let path = CString::new("IODeviceTree:/product").ok()?;
        let entry = IORegistryEntryFromPath(0, path.as_ptr());
        if entry == 0 {
            return None;
        }
        let key_c = CString::new("product-name").ok()?;
        let key = CFStringCreateWithCString(std::ptr::null(), key_c.as_ptr(), UTF8);
        let mut out = None;
        if !key.is_null() {
            let data = IORegistryEntryCreateCFProperty(entry, key, std::ptr::null(), 0);
            if !data.is_null() {
                if CFGetTypeID(data) == CFDataGetTypeID() {
                    let len = CFDataGetLength(data).clamp(0, 256) as usize;
                    let ptr = CFDataGetBytePtr(data);
                    if !ptr.is_null() {
                        let bytes = std::slice::from_raw_parts(ptr, len);
                        let s = String::from_utf8_lossy(bytes).trim_end_matches('\0').trim().to_string();
                        if !s.is_empty() {
                            out = Some(s);
                        }
                    }
                }
                CFRelease(data);
            }
            CFRelease(key);
        }
        IOObjectRelease(entry);
        out
    }
}

/// Title shown in the window: "MacBook Pro (16-inch, 2021) · M1 Max", falling back to the
/// model identifier ("Mac17,2") when macOS doesn't store a marketing name.
pub fn machine_title() -> String {
    let name = product_name().unwrap_or_else(crate::smc::hw_model);
    let brand = crate::smc::sysctl_string("machdep.cpu.brand_string");
    match brand.strip_prefix("Apple ") {
        Some(chip) if !chip.is_empty() => format!("{name} · {chip}"),
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn machine_title_is_readable() {
        let t = super::machine_title();
        println!("title: {t}");
        assert!(!t.is_empty());
        if crate::smc::sysctl_string("machdep.cpu.brand_string").starts_with("Apple") {
            assert!(super::product_name().is_some(), "Apple silicon Macs store a product name");
            assert!(t.contains(" · "));
        }
    }
}
