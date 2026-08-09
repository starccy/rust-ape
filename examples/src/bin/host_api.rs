//! Calling host-specific APIs, picked at runtime instead of by `cfg`.
//!
//! Everything here is compiled into every copy of the binary. cosmo works out
//! which host it landed on at startup, `ape::is_windows()` and friends are
//! plain runtime tests, and the branch nobody takes just sits there. That is
//! the whole trick, and it is also the whole danger: the compiler cannot tell
//! you that you called a Win32 function on Linux. It will be a SIGILL.
//!
//! Three ways to reach Win32, in ascending order of effort:
//!
//!   1. cosmo generates a SysV thunk for most of what it imports, so a plain
//!      `extern "C"` block reaches it. `GetCurrentProcessId` below.
//!   2. For functions with `A`/`W` variants cosmo drops the suffix and binds
//!      the wide one, so the thunk is `GetSystemDirectory` while the import is
//!      `GetSystemDirectoryW`. `#[link_name]` bridges that.
//!   3. Anything cosmo never imported needs `LoadLibraryA` and
//!      `GetProcAddress`, and the pointer that comes back wants Microsoft's
//!      x64 convention rather than the SysV one cosmo's thunks present.
//!
//! `extern "win64"` only exists on x86_64, and an APE binary is compiled for
//! arm64 as well, so step 3 has to be gated on the arch or the arm64 half
//! won't build.
//!
//! The last section is about callbacks, which is where the ceiling is lower
//! than it looks. Read it before designing anything around them.

use std::ffi::{c_char, c_void};

fn main() {
    println!("host: {:?}, arch: {}", ape::current_os(), std::env::consts::ARCH);

    if ape::is_windows() {
        windows();
    } else {
        elsewhere();
    }

    println!("\nhost api ok");
}

// ---------------------------------------------------------------- Windows

unsafe extern "C" {
    // Group 1: cosmo imports it and the thunk carries the same name.
    fn GetCurrentProcessId() -> u32;
    // Group 2: the import is GetSystemDirectoryW, the thunk is unsuffixed.
    #[link_name = "GetSystemDirectory"]
    fn GetSystemDirectoryW(buf: *mut u16, size: u32) -> u32;
}

// Only the sections below reach these, and those are x86_64 only.
#[cfg(target_arch = "x86_64")]
unsafe extern "C" {
    fn LoadLibraryA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;

    fn CreateThread(sec: *mut c_void, stack: usize, start: *mut c_void, param: *mut c_void,
                    flags: u32, tid: *mut u32) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, ms: u32) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn SetEvent(handle: *mut c_void) -> i32;
}

fn windows() {
    unsafe {
        let pid = GetCurrentProcessId();
        assert_eq!(pid, std::process::id(), "Win32 and std disagree about our own pid");
        println!("GetCurrentProcessId agrees with std::process::id(): {pid}");

        let mut buf = [0u16; 260];
        let n = GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) as usize;
        assert!(n > 0 && n < buf.len(), "GetSystemDirectory returned {n}");
        println!("GetSystemDirectory: {}", String::from_utf16_lossy(&buf[..n]));
    }

    #[cfg(target_arch = "x86_64")]
    {
        by_procaddress();
        doorbell();
    }
    #[cfg(not(target_arch = "x86_64"))]
    println!("the GetProcAddress and callback sections need the win64 ABI, which is x86_64 only");
}

/// A function cosmo never imported, so there is no symbol to link against.
#[cfg(target_arch = "x86_64")]
fn by_procaddress() {
    unsafe {
        let k32 = LoadLibraryA(c"kernel32.dll".as_ptr() as *const u8);
        assert!(!k32.is_null(), "LoadLibraryA(kernel32.dll) failed");

        let p = GetProcAddress(k32, c"GetPhysicallyInstalledSystemMemory".as_ptr() as *const u8);
        assert!(!p.is_null(), "GetProcAddress came back null");

        let f: unsafe extern "win64" fn(*mut u64) -> i32 = std::mem::transmute(p);
        let mut kb = 0u64;
        let ok = f(&mut kb);
        assert!(ok != 0 && kb > 0, "GetPhysicallyInstalledSystemMemory said {ok}, {kb} KiB");
        println!("GetPhysicallyInstalledSystemMemory via GetProcAddress: {} MiB", kb / 1024);
    }
}

/// Callbacks, and the reason this one only rings a bell.
///
/// The host can call back into us and the arguments arrive intact. What the
/// callback may then do depends on who created the thread. On a thread cosmo
/// set up, everything works. On a thread the host created, which is every
/// `CALLBACK_FUNCTION` audio callback, window procedure and IO completion
/// routine, cosmo's libc is off limits: its thread block was never installed
/// there and even a bare `write` faults.
///
/// Win32 itself is still fine, because cosmo's thunks only shuffle registers.
/// So the callback stores what it has and rings an event, and a thread cosmo
/// owns does the real work. Audio and GUI APIs take `CALLBACK_EVENT` and
/// `CALLBACK_WINDOW` for exactly this reason.
#[cfg(target_arch = "x86_64")]
fn doorbell() {
    use std::sync::atomic::{AtomicU64, Ordering};

    static EVENT: AtomicU64 = AtomicU64::new(0);
    static PAYLOAD: AtomicU64 = AtomicU64::new(0);
    const TOKEN: u64 = 0xc0ffee;

    unsafe extern "win64" fn ring(param: *mut c_void) -> u32 {
        // Atomics and Win32, nothing else. No allocation, no formatting, no
        // println!, no libc.
        PAYLOAD.store(param as u64, Ordering::SeqCst);
        unsafe { SetEvent(EVENT.load(Ordering::SeqCst) as *mut c_void) };
        0
    }

    unsafe {
        let k32 = LoadLibraryA(c"kernel32.dll".as_ptr() as *const u8);
        let p = GetProcAddress(k32, c"CreateEventW".as_ptr() as *const u8);
        assert!(!p.is_null(), "GetProcAddress(CreateEventW) came back null");
        let create_event: unsafe extern "win64" fn(*mut c_void, i32, i32, *const u16) -> *mut c_void =
            std::mem::transmute(p);

        let ev = create_event(std::ptr::null_mut(), 1, 0, std::ptr::null());
        assert!(!ev.is_null(), "CreateEventW failed");
        EVENT.store(ev as u64, Ordering::SeqCst);

        let mut tid = 0;
        let thread = CreateThread(std::ptr::null_mut(), 0, ring as *mut c_void,
                                  TOKEN as *mut c_void, 0, &mut tid);
        assert!(!thread.is_null(), "CreateThread failed");

        // Back on our own thread, so the whole libc is available again.
        let waited = WaitForSingleObject(ev, 10_000);
        assert_eq!(waited, 0, "the callback never rang, WaitForSingleObject said {waited}");
        assert_eq!(PAYLOAD.load(Ordering::SeqCst), TOKEN, "the callback got the wrong argument");
        println!("callback on a host thread rang the doorbell, handed over 0x{TOKEN:x}");

        WaitForSingleObject(thread, 10_000);
        CloseHandle(thread);
        CloseHandle(ev);
    }
}

// ------------------------------------------------------------- everywhere else

unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: i32) -> *mut c_void;
    fn dlerror() -> *mut c_char;
    fn cosmo_dlopen(path: *const c_char, flags: i32) -> *mut c_void;
    fn cosmo_dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn cosmo_dlerror() -> *mut c_char;
    fn cosmo_dltramp(func: *mut c_void) -> *mut c_void;
    fn cosmo_dlclose(handle: *mut c_void) -> i32;
}

fn cstr(p: *mut c_char) -> String {
    if p.is_null() {
        return "(no message)".into();
    }
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn elsewhere() {
    println!("the Win32 section above is in this binary and never ran");

    // Plain dlopen is not the way in. cosmo answers with a message saying so,
    // which is worth showing rather than describing.
    let h = unsafe { dlopen(c"libm.so.6".as_ptr(), 1) };
    if h.is_null() {
        println!("dlopen says: {}", cstr(unsafe { dlerror() }));
    } else {
        println!("dlopen worked on this host, which is newer behaviour than expected");
    }

    // cosmo_dlopen is. Whether any particular library exists is the machine's
    // business, so this reports instead of asserting: a CI runner need not
    // have a sound card's userspace, and macOS x86-64 has no support for this
    // at all.
    let candidates: &[&std::ffi::CStr] = if ape::is_xnu() {
        &[c"libSystem.B.dylib", c"/usr/lib/libSystem.B.dylib"]
    } else {
        &[c"libm.so.6", c"libm.so", c"libc.so.6"]
    };

    for name in candidates {
        let lib = unsafe { cosmo_dlopen(name.as_ptr(), 1) };
        if lib.is_null() {
            println!("cosmo_dlopen({name:?}) -> {}", cstr(unsafe { cosmo_dlerror() }));
            continue;
        }
        // Every symbol has to go through the trampoline before it is called:
        // the foreign library expects the host's calling convention and TLS,
        // not cosmo's.
        let sym = unsafe { cosmo_dlsym(lib, c"sqrt".as_ptr()) };
        if sym.is_null() {
            println!("cosmo_dlopen({name:?}) ok, but it has no sqrt");
        } else {
            let sqrt: extern "C" fn(f64) -> f64 =
                unsafe { std::mem::transmute(cosmo_dltramp(sym)) };
            let got = sqrt(2.0);
            assert!((got - std::f64::consts::SQRT_2).abs() < 1e-12,
                    "the host's sqrt(2.0) came back {got}");
            println!("cosmo_dlopen({name:?}) -> sqrt(2.0) = {got}");
        }
        unsafe { cosmo_dlclose(lib) };
        return;
    }

    println!("no candidate library loaded here, which is a fact about this machine");
}
