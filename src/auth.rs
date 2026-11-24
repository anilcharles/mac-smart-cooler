//! Admin authorization through macOS Authorization Services.
//! This is the system "… wants to make changes" sheet, which offers Touch ID.

use std::ffi::{c_char, c_void, CString};
use std::ptr::{null, null_mut};

#[repr(C)]
struct Item {
    name: *const c_char,
    value_length: usize,
    value: *mut c_void,
    flags: u32,
}

#[repr(C)]
struct ItemSet {
    count: u32,
    items: *mut Item,
}

type AuthRef = *mut c_void;

#[link(name = "Security", kind = "framework")]
extern "C" {
    fn AuthorizationCreate(
        rights: *const ItemSet,
        env: *const ItemSet,
        flags: u32,
        out: *mut AuthRef,
    ) -> i32;
    fn AuthorizationExecuteWithPrivileges(
        auth: AuthRef,
        path: *const c_char,
        options: u32,
        args: *const *const c_char,
        pipe: *mut *mut c_void,
    ) -> i32;
    fn AuthorizationFree(auth: AuthRef, flags: u32) -> i32;
}

extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

const INTERACTION_ALLOWED: u32 = 1;
const EXTEND_RIGHTS: u32 = 2;
const PRE_AUTHORIZE: u32 = 16;
const DESTROY_RIGHTS: u32 = 8;
const ERR_CANCELED: i32 = -60006;
/// Error prefix when Authorization Services can't launch the tool at all (only then may
/// a caller fall back to another prompt).
pub const CANNOT_START: &str = "could not start privileged tool";

/// Run "exe args..." as root after the system admin prompt (Touch ID or password).
/// Ok(true) = ran and exited successfully, Ok(false) = user cancelled.
pub fn run_as_root(
    exe: &str,
    args: &[&str],
    prompt: &str,
    icon: Option<&str>,
) -> Result<bool, String> {
    let right = CString::new("system.privilege.admin").unwrap();
    let mut right_item = Item {
        name: right.as_ptr(),
        value_length: 0,
        value: null_mut(),
        flags: 0,
    };
    let rights = ItemSet {
        count: 1,
        items: &mut right_item,
    };

    let (pk, pv) = (
        CString::new("prompt").unwrap(),
        CString::new(prompt).map_err(|e| e.to_string())?,
    );
    let ik = CString::new("icon").unwrap();
    let iv = icon.and_then(|i| CString::new(i).ok());
    let mut env_items = vec![Item {
        name: pk.as_ptr(),
        value_length: prompt.len(),
        value: pv.as_ptr() as *mut c_void,
        flags: 0,
    }];
    if let Some(iv) = &iv {
        env_items.push(Item {
            name: ik.as_ptr(),
            value_length: iv.as_bytes().len(),
            value: iv.as_ptr() as *mut c_void,
            flags: 0,
        });
    }
    let env = ItemSet {
        count: env_items.len() as u32,
        items: env_items.as_mut_ptr(),
    };

    let mut auth: AuthRef = null_mut();
    let st = unsafe {
        AuthorizationCreate(
            &rights,
            &env,
            INTERACTION_ALLOWED | EXTEND_RIGHTS | PRE_AUTHORIZE,
            &mut auth,
        )
    };
    if st == ERR_CANCELED {
        return Ok(false);
    }
    if st != 0 {
        return Err(format!("authorization failed ({st})"));
    }

    let path = CString::new(exe).map_err(|e| e.to_string())?;
    let cargs: Vec<CString> = args.iter().filter_map(|a| CString::new(*a).ok()).collect();
    let mut argv: Vec<*const c_char> = cargs.iter().map(|a| a.as_ptr()).collect();
    argv.push(null());
    let st = unsafe {
        AuthorizationExecuteWithPrivileges(auth, path.as_ptr(), 0, argv.as_ptr(), null_mut())
    };
    let mut ok = st == 0;
    if ok {
        // the privileged tool runs as our child; wait for it and use its exit status
        let mut status = 0;
        let pid = unsafe { waitpid(-1, &mut status, 0) };
        ok = pid > 0 && (status & 0x7f) == 0 && ((status >> 8) & 0xff) == 0;
    }
    unsafe { AuthorizationFree(auth, DESTROY_RIGHTS) };
    if st != 0 {
        return Err(format!("{CANNOT_START} ({st})"));
    }
    if !ok {
        return Err("installing the helper failed".into());
    }
    Ok(true)
}
