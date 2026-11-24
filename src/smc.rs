//! Minimal AppleSMC client over IOKit (works on Intel and Apple Silicon).

use std::collections::HashMap;
use std::ffi::{c_char, c_void, CString};

pub type Res<T> = Result<T, String>;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct KeyDataVers {
    major: u8,
    minor: u8,
    build: u8,
    reserved: u8,
    release: u16,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PLimitData {
    version: u16,
    length: u16,
    cpu_p_limit: u32,
    gpu_p_limit: u32,
    mem_p_limit: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct KeyInfo {
    pub data_size: u32,
    pub data_type: u32,
    pub data_attributes: u8,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct KeyData {
    key: u32,
    vers: KeyDataVers,
    p_limit: PLimitData,
    key_info: KeyInfo,
    result: u8,
    status: u8,
    data8: u8,
    data32: u32,
    bytes: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<KeyData>() == 80);

const KERNEL_INDEX_SMC: u32 = 2;
const CMD_READ_BYTES: u8 = 5;
const CMD_WRITE_BYTES: u8 = 6;
const CMD_READ_INDEX: u8 = 8;
const CMD_READ_KEYINFO: u8 = 9;
const IO_RETURN_NOT_PRIVILEGED: i32 = 0xe00002c1_u32 as i32;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
    fn IOServiceOpen(service: u32, owning_task: u32, typ: u32, connect: *mut u32) -> i32;
    fn IOServiceClose(connect: u32) -> i32;
    fn IOObjectRelease(obj: u32) -> i32;
    fn IOConnectCallStructMethod(
        connection: u32,
        selector: u32,
        input: *const c_void,
        input_size: usize,
        output: *mut c_void,
        output_size: *mut usize,
    ) -> i32;
}

extern "C" {
    static mach_task_self_: u32;
    fn geteuid() -> u32;
    fn sysctlbyname(
        name: *const c_char,
        oldp: *mut c_void,
        oldlenp: *mut usize,
        newp: *mut c_void,
        newlen: usize,
    ) -> i32;
}

pub fn is_root() -> bool {
    unsafe { geteuid() == 0 }
}

/// e.g. "MacBookPro18,2"
pub fn hw_model() -> String {
    let m = sysctl_string("hw.model");
    if m.is_empty() { "Mac".into() } else { m }
}

/// A string sysctl value ("" if it can't be read).
pub fn sysctl_string(name: &str) -> String {
    let Ok(name) = CString::new(name) else { return String::new() };
    let mut buf = [0u8; 256];
    let mut len = buf.len();
    let r = unsafe {
        sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr() as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 {
        return String::new();
    }
    String::from_utf8_lossy(&buf[..len.min(buf.len())])
        .trim_end_matches('\0')
        .trim()
        .to_string()
}

pub fn fourcc(key: &str) -> Res<u32> {
    let b = key.as_bytes();
    if b.len() != 4 {
        return Err(format!("SMC key must be 4 chars: {key:?}"));
    }
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn fourcc_str(v: u32) -> String {
    String::from_utf8_lossy(&v.to_be_bytes()).to_string()
}

#[derive(Debug, Clone)]
pub struct Value {
    pub ty: String,
    pub bytes: Vec<u8>,
}

fn hex_digit(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        let b = &self.bytes;
        let t = self.ty.as_bytes();
        match self.ty.as_str() {
            "flt " if b.len() >= 4 => Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
            "ui8 " | "flag" | "char" if !b.is_empty() => Some(b[0] as f64),
            "si8 " if !b.is_empty() => Some(b[0] as i8 as f64),
            "ui16" if b.len() >= 2 => Some(u16::from_be_bytes([b[0], b[1]]) as f64),
            "si16" if b.len() >= 2 => Some(i16::from_be_bytes([b[0], b[1]]) as f64),
            "ui32" if b.len() >= 4 => Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64),
            "ioft" if b.len() >= 8 => {
                let v = u64::from_le_bytes(b[..8].try_into().ok()?);
                Some(v as f64 / 65536.0)
            }
            _ if t.len() == 4 && b.len() >= 2 && (t[0..2] == *b"fp" || t[0..2] == *b"sp") => {
                let frac = hex_digit(t[3])?;
                let div = (1u32 << frac) as f64;
                if t[0] == b'f' {
                    Some(u16::from_be_bytes([b[0], b[1]]) as f64 / div)
                } else {
                    Some(i16::from_be_bytes([b[0], b[1]]) as f64 / div)
                }
            }
            _ => None,
        }
    }
}

fn encode(ty: &str, size: usize, v: f64) -> Res<Vec<u8>> {
    let t = ty.as_bytes();
    let out = match ty {
        "flt " => (v as f32).to_le_bytes().to_vec(),
        "ui8 " | "flag" => vec![v.clamp(0.0, 255.0) as u8],
        "ui16" => (v as u16).to_be_bytes().to_vec(),
        "ui32" => (v as u32).to_be_bytes().to_vec(),
        _ if t.len() == 4 && t[0..2] == *b"fp" => {
            let frac = hex_digit(t[3]).ok_or("bad fp type")?;
            ((v * (1u32 << frac) as f64) as u16).to_be_bytes().to_vec()
        }
        _ => return Err(format!("don't know how to encode SMC type {ty:?}")),
    };
    if out.len() != size {
        return Err(format!(
            "size mismatch for type {ty:?}: {} vs {size}",
            out.len()
        ));
    }
    Ok(out)
}

/// The SMC operations fan control needs. The real `Smc` implements it; tests use
/// simulated Macs that behave like the real hardware (see fans.rs tests).
pub trait SmcIo {
    fn has_key(&mut self, key: &str) -> bool;
    fn read_f64(&mut self, key: &str) -> Option<f64>;
    fn write_f64(&mut self, key: &str, v: f64) -> Res<()>;
    fn write_bytes(&mut self, key: &str, bytes: &[u8]) -> Res<()>;
    /// raw value bytes (None if the key can't be read)
    fn read_bytes(&mut self, key: &str) -> Option<Vec<u8>>;
}

impl SmcIo for Smc {
    fn has_key(&mut self, key: &str) -> bool {
        Smc::has_key(self, key)
    }
    fn read_f64(&mut self, key: &str) -> Option<f64> {
        Smc::read_f64(self, key)
    }
    fn write_f64(&mut self, key: &str, v: f64) -> Res<()> {
        Smc::write_f64(self, key, v)
    }
    fn write_bytes(&mut self, key: &str, bytes: &[u8]) -> Res<()> {
        Smc::write_bytes(self, key, bytes)
    }
    fn read_bytes(&mut self, key: &str) -> Option<Vec<u8>> {
        Smc::read(self, key).ok().map(|v| v.bytes)
    }
}

pub struct Smc {
    conn: u32,
    info_cache: HashMap<u32, KeyInfo>,
}

// The connection handle is a plain mach port number; IOKit calls are thread safe.
unsafe impl Send for Smc {}

impl Smc {
    pub fn open() -> Res<Self> {
        unsafe {
            let name = CString::new("AppleSMC").unwrap();
            let matching = IOServiceMatching(name.as_ptr());
            if matching.is_null() {
                return Err("IOServiceMatching(AppleSMC) failed".into());
            }
            let svc = IOServiceGetMatchingService(0, matching);
            if svc == 0 {
                return Err("AppleSMC service not found".into());
            }
            let mut conn = 0u32;
            let kr = IOServiceOpen(svc, mach_task_self_, 0, &mut conn);
            IOObjectRelease(svc);
            if kr != 0 {
                return Err(format!("IOServiceOpen failed: {kr:#x}"));
            }
            Ok(Smc {
                conn,
                info_cache: HashMap::new(),
            })
        }
    }

    fn call(&self, input: &KeyData) -> Res<KeyData> {
        let mut out = KeyData::default();
        let mut out_size = std::mem::size_of::<KeyData>();
        let kr = unsafe {
            IOConnectCallStructMethod(
                self.conn,
                KERNEL_INDEX_SMC,
                input as *const KeyData as *const c_void,
                std::mem::size_of::<KeyData>(),
                &mut out as *mut KeyData as *mut c_void,
                &mut out_size,
            )
        };
        if kr == IO_RETURN_NOT_PRIVILEGED {
            return Err("not privileged (run with sudo)".into());
        }
        if kr != 0 {
            return Err(format!("IOKit error {kr:#x}"));
        }
        if out.result != 0 {
            return Err(format!("SMC error {}", out.result));
        }
        Ok(out)
    }

    pub fn key_info(&mut self, key: u32) -> Res<KeyInfo> {
        if let Some(i) = self.info_cache.get(&key) {
            return Ok(*i);
        }
        let input = KeyData {
            key,
            data8: CMD_READ_KEYINFO,
            ..Default::default()
        };
        let info = self.call(&input)?.key_info;
        self.info_cache.insert(key, info);
        Ok(info)
    }

    pub fn has_key(&mut self, key: &str) -> bool {
        fourcc(key).and_then(|k| self.key_info(k)).is_ok()
    }

    pub fn read(&mut self, key: &str) -> Res<Value> {
        let k = fourcc(key)?;
        let info = self.key_info(k)?;
        let mut input = KeyData {
            key: k,
            data8: CMD_READ_BYTES,
            ..Default::default()
        };
        input.key_info.data_size = info.data_size;
        let out = self.call(&input)?;
        let n = (info.data_size as usize).min(32);
        Ok(Value {
            ty: fourcc_str(info.data_type),
            bytes: out.bytes[..n].to_vec(),
        })
    }

    pub fn read_f64(&mut self, key: &str) -> Option<f64> {
        self.read(key).ok()?.as_f64()
    }

    pub fn write_bytes(&mut self, key: &str, bytes: &[u8]) -> Res<()> {
        let k = fourcc(key)?;
        let info = self.key_info(k)?;
        if bytes.len() != info.data_size as usize || bytes.len() > 32 {
            return Err(format!(
                "{key}: expected {} bytes, got {}",
                info.data_size,
                bytes.len()
            ));
        }
        let mut input = KeyData {
            key: k,
            data8: CMD_WRITE_BYTES,
            ..Default::default()
        };
        input.key_info.data_size = info.data_size;
        input.bytes[..bytes.len()].copy_from_slice(bytes);
        self.call(&input)
            .map(|_| ())
            .map_err(|e| format!("write {key}: {e}"))
    }

    pub fn write_f64(&mut self, key: &str, v: f64) -> Res<()> {
        let k = fourcc(key)?;
        let info = self.key_info(k)?;
        let bytes = encode(&fourcc_str(info.data_type), info.data_size as usize, v)?;
        self.write_bytes(key, &bytes)
    }

    pub fn key_count(&mut self) -> u32 {
        self.read_f64("#KEY").unwrap_or(0.0) as u32
    }

    pub fn key_at(&self, index: u32) -> Res<String> {
        let input = KeyData {
            data8: CMD_READ_INDEX,
            data32: index,
            ..Default::default()
        };
        Ok(fourcc_str(self.call(&input)?.key))
    }
}

impl Drop for Smc {
    fn drop(&mut self) {
        unsafe {
            IOServiceClose(self.conn);
        }
    }
}
