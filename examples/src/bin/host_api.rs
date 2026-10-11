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
//! The last section is a callback on a thread Windows starts, which runs Rust
//! like any other thread. For libraries rather than Win32 itself, see
//! host_dlopen.rs and `#[ape::host_library]`.

use std::ffi::{c_char, c_void};

fn main() {
    println!(
        "host: {:?}, arch: {}",
        ape::current_os(),
        std::env::consts::ARCH
    );

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

    fn CreateThread(
        sec: *mut c_void,
        stack: usize,
        start: *mut c_void,
        param: *mut c_void,
        flags: u32,
        tid: *mut u32,
    ) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, ms: u32) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

fn windows() {
    unsafe {
        let pid = GetCurrentProcessId();
        assert_eq!(
            pid,
            std::process::id(),
            "Win32 and std disagree about our own pid"
        );
        println!("GetCurrentProcessId agrees with std::process::id(): {pid}");

        let mut buf = [0u16; 260];
        let n = GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) as usize;
        assert!(n > 0 && n < buf.len(), "GetSystemDirectory returned {n}");
        println!(
            "GetSystemDirectory: {}",
            String::from_utf16_lossy(&buf[..n])
        );
    }

    #[cfg(target_arch = "x86_64")]
    {
        by_procaddress();
        host_thread_callback();
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

        let p = GetProcAddress(
            k32,
            c"GetPhysicallyInstalledSystemMemory".as_ptr() as *const u8,
        );
        assert!(!p.is_null(), "GetProcAddress came back null");

        let f: unsafe extern "win64" fn(*mut u64) -> i32 = std::mem::transmute(p);
        let mut kb = 0u64;
        let ok = f(&mut kb);
        assert!(
            ok != 0 && kb > 0,
            "GetPhysicallyInstalledSystemMemory said {ok}, {kb} KiB"
        );
        println!(
            "GetPhysicallyInstalledSystemMemory via GetProcAddress: {} MiB",
            kb / 1024
        );
    }
}

/// A callback on a thread the host created.
///
/// Every `CALLBACK_FUNCTION` audio callback, window procedure and IO completion
/// routine runs on a thread like this. The thread gets cosmo's thread state
/// the first time it runs our code, so the callback can allocate, format and
/// print, and its thread locals are dropped when the thread exits.
#[cfg(target_arch = "x86_64")]
fn host_thread_callback() {
    use std::cell::Cell;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DROPS: AtomicUsize = AtomicUsize::new(0);
    static SEEN: Mutex<Option<String>> = Mutex::new(None);

    struct Counted(Cell<u64>);
    impl Drop for Counted {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    thread_local! {
        static LOCAL: Counted = const { Counted(Cell::new(0)) };
    }

    unsafe extern "win64" fn callback(param: *mut c_void) -> u32 {
        LOCAL.with(|l| l.0.set(param as u64));
        let msg = format!(
            "hello from host thread {:?}, param 0x{:x}",
            std::thread::current().id(),
            param as u64
        );
        println!("  {msg}");
        *SEEN.lock().unwrap() = Some(msg);
        0
    }

    unsafe {
        let mut tid = 0;
        let thread = CreateThread(
            std::ptr::null_mut(),
            0,
            callback as *mut c_void,
            0xc0ffee as *mut c_void,
            0,
            &mut tid,
        );
        assert!(!thread.is_null(), "CreateThread failed");
        let waited = WaitForSingleObject(thread, 10_000);
        assert_eq!(
            waited, 0,
            "the host thread never finished, WaitForSingleObject said {waited}"
        );
        CloseHandle(thread);
    }
    assert!(SEEN.lock().unwrap().is_some(), "the callback didn't run");
    assert_eq!(
        DROPS.load(Ordering::SeqCst),
        1,
        "the thread local wasn't dropped"
    );
    println!("callback on a host thread ran Rust code, and its thread local was dropped");
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
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
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
            println!(
                "cosmo_dlopen({name:?}) -> {}",
                cstr(unsafe { cosmo_dlerror() })
            );
            continue;
        }
        // cosmo_dltramp makes sure the calling thread runs with the host's
        // thread block, which the library's own code expects.
        let sym = unsafe { cosmo_dlsym(lib, c"sqrt".as_ptr()) };
        if sym.is_null() {
            println!("cosmo_dlopen({name:?}) ok, but it has no sqrt");
        } else {
            let sqrt: extern "C" fn(f64) -> f64 =
                unsafe { std::mem::transmute(cosmo_dltramp(sym)) };
            let got = sqrt(2.0);
            assert!(
                (got - std::f64::consts::SQRT_2).abs() < 1e-12,
                "the host's sqrt(2.0) came back {got}"
            );
            println!("cosmo_dlopen({name:?}) -> sqrt(2.0) = {got}");
        }
        unsafe { cosmo_dlclose(lib) };
        return;
    }

    println!("no candidate library loaded here, which is a fact about this machine");
}
