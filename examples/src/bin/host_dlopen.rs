//! Calling host C libraries, and being called back by them, on any thread.
//!
//! The libraries here are the ones every host ships: its C library and math
//! library, and on Windows the Universal CRT and kernel32. What each check
//! covers:
//!
//! - qsort() with a Rust comparator, which runs on the calling thread and
//!   allocates and prints from inside the host's qsort, and whose backtrace
//!   goes on through the host's frames.
//! - threads the host starts itself (pthread_create, CreateThread) running
//!   Rust code: println!, a Mutex, and a thread_local! whose Drop has to run
//!   when the host thread exits.
//! - the host's own errno, which is not ours.
//! - double and mixed int/double arguments, which the Windows convention
//!   passes differently from the SysV one, and a variadic call with both
//!   kinds among the variadic arguments, which Apple Silicon puts on the
//!   stack.
//! - a Rust function exported by name, which the host's own dlsym() (or
//!   GetProcAddress() on Windows) then finds, the way a plugin finds its
//!   host's API; a variadic one, called with the host's layout of the
//!   arguments; and a variable, one the host writes and we read.

use std::cell::Cell;
use std::ffi::{c_char, c_double, c_int, c_uint, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use ape::host::Callback;

#[ape::host_library(static LIBC = linux ["libc.so.6", "libc.musl-x86_64.so.1"],
                    macos ["/usr/lib/libSystem.B.dylib"],
                    windows ["ucrtbase.dll", "msvcrt.dll"],
                    bsd ["libc.so.7", "libc.so"])]
unsafe extern "C" {
    fn qsort(base: *mut c_void, n: usize, size: usize, cmp: Callback);
    fn strlen(s: *const c_char) -> usize;
    fn getenv(name: *const c_char) -> *const c_char;
}

#[ape::host_library(static LIBM = linux ["libm.so.6"], macos ["/usr/lib/libSystem.B.dylib"],
                    windows ["ucrtbase.dll", "msvcrt.dll"], bsd ["libm.so.5", "libm.so"])]
unsafe extern "C" {
    fn pow(x: c_double, y: c_double) -> c_double;
    fn ldexp(x: c_double, exp: c_int) -> c_double;
}

// access() on unix, _access() in the CRT
#[ape::host_library(static POSIX = linux ["libc.so.6"], macos ["/usr/lib/libSystem.B.dylib"],
                    bsd ["libc.so.7", "libc.so"])]
unsafe extern "C" {
    fn access(path: *const c_char, mode: c_int) -> c_int;
    fn setenv(name: *const c_char, value: *const c_char, overwrite: c_int) -> c_int;
    fn unsetenv(name: *const c_char) -> c_int;
}

#[ape::host_library(static CRT = windows ["ucrtbase.dll"])]
unsafe extern "C" {
    fn _access(path: *const c_char, mode: c_int) -> c_int;
    fn _putenv_s(name: *const c_char, value: *const c_char) -> c_int;
}

// pthread_create moved into libc.so.6 in glibc 2.34, before that it's in
// libpthread.so.0, whose dlsym also searches libc.so.6 on newer glibc
#[ape::host_library(static PTHREAD = linux ["libpthread.so.0", "libc.so.6"],
                    macos ["/usr/lib/libSystem.B.dylib"], bsd ["libthr.so.3", "libpthread.so"])]
unsafe extern "C" {
    fn pthread_create(
        th: *mut usize,
        attr: *const c_void,
        start: Callback,
        arg: *mut c_void,
    ) -> c_int;
    fn pthread_join(th: usize, ret: *mut *mut c_void) -> c_int;
}

#[ape::host_library(static DL = linux ["libdl.so.2", "libc.so.6"], macos ["/usr/lib/libSystem.B.dylib"],
                    bsd ["libc.so.7", "libc.so"])]
unsafe extern "C" {
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

// the variadic functions, which are called through host_variadic!
#[ape::host_library(static FMT = linux ["libc.so.6", "libc.musl-x86_64.so.1", "libc.musl-aarch64.so.1"],
                    macos ["/usr/lib/libSystem.B.dylib"], bsd ["libc.so.7", "libc.so"])]
unsafe extern "C" {
    fn snprintf(buf: *mut c_char, n: usize, fmt: *const c_char, ...) -> c_int;
}

// the CRT that every Windows has, whose snprintf goes by an old name
#[ape::host_library(static MSVCRT = windows ["msvcrt.dll"])]
unsafe extern "C" {
    fn _snprintf(buf: *mut c_char, n: usize, fmt: *const c_char, ...) -> c_int;
}

#[ape::host_library(static KERNEL32 = windows ["kernel32.dll"])]
unsafe extern "C" {
    fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    fn CreateThread(
        sec: *mut c_void,
        stack: usize,
        start: Callback,
        arg: *mut c_void,
        flags: c_uint,
        tid: *mut c_uint,
    ) -> *mut c_void;
    fn WaitForSingleObject(h: *mut c_void, ms: c_uint) -> c_uint;
    fn CloseHandle(h: *mut c_void) -> c_int;
    fn SetEnvironmentVariableW(name: *const u16, value: *const u16) -> c_int;
    fn GetEnvironmentVariableW(name: *const u16, buf: *mut u16, size: c_uint) -> c_uint;
}

static COMPARES: AtomicUsize = AtomicUsize::new(0);
static FRAMES_IN_CALLBACK: AtomicUsize = AtomicUsize::new(0);

// how many frames a backtrace taken here has
#[inline(never)]
fn frames() -> usize {
    let bt = std::backtrace::Backtrace::force_capture().to_string();
    bt.lines()
        .filter(|l| {
            l.trim_start()
                .split(':')
                .next()
                .is_some_and(|n| n.parse::<u32>().is_ok())
        })
        .count()
}

#[ape::host_callback]
fn compare(a: *const c_void, b: *const c_void) -> c_int {
    if COMPARES.fetch_add(1, Ordering::Relaxed) == 0 {
        let s = String::from("comparator runs inside the host's qsort");
        println!("  {s}");
        FRAMES_IN_CALLBACK.store(frames(), Ordering::Relaxed);
    }
    let (a, b) = unsafe { (*(a as *const i32), *(b as *const i32)) };
    a.cmp(&b) as c_int
}

static BODIES: AtomicUsize = AtomicUsize::new(0);
static DROPS: AtomicUsize = AtomicUsize::new(0);
static SEEN: Mutex<Vec<usize>> = Mutex::new(Vec::new());

struct Counted(Cell<usize>);

impl Drop for Counted {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::Relaxed);
    }
}

thread_local! {
    static PER_THREAD: Counted = const { Counted(Cell::new(0)) };
}

fn thread_body(i: usize) {
    PER_THREAD.with(|c| c.0.set(i));
    let v: Vec<usize> = (0..i % 7 + 1).collect();
    SEEN.lock().unwrap().push(v.len());
    if i == 0 {
        println!(
            "  first host thread: std sees it as {:?}",
            std::thread::current().id()
        );
    }
    BODIES.fetch_add(1, Ordering::Relaxed);
}

ape::host_exports! {
    static DEMO = "libape-demo.so", windows "ape-demo.dll";
    fn ape_hello(x: c_int) -> c_int {
        x * 2 + 1
    }
    // n pairs of an int and a double follow
    fn ape_mix(n: c_int, mut args: ...) -> c_double {
        let mut sum = 0.0;
        for _ in 0..n {
            let i = unsafe { args.next_arg::<c_int>() };
            let d = unsafe { args.next_arg::<c_double>() };
            sum += i as f64 * d;
        }
        sum
    }
    // eight fixed parameters, the most a trampoline passes on macOS and
    // more than Windows passes in registers, then n doubles
    fn ape_mix8(n: c_int, a1: c_int, a2: c_int, a3: c_int, a4: c_int, a5: c_int, a6: c_int, a7: c_int, mut args: ...) -> c_double {
        let mut sum = (a1 + a2 * 2 + a3 * 3 + a4 * 4 + a5 * 5 + a6 * 6 + a7 * 7) as f64;
        for _ in 0..n {
            sum += unsafe { args.next_arg::<c_double>() };
        }
        sum
    }
    static ape_counter: c_int = 7;
}

// the exported library as a plugin sees it, by the name it was exported
// under; the variadic call then takes the host's own route in
mod plugin_view {
    use super::*;
    #[ape::host_library(static DEMO_BACK = linux ["libape-demo.so"], macos ["libape-demo.so"], windows ["ape-demo.dll"])]
    unsafe extern "C" {
        pub fn ape_mix(n: c_int, ...) -> c_double;
        pub fn ape_mix8(
            n: c_int,
            a1: c_int,
            a2: c_int,
            a3: c_int,
            a4: c_int,
            a5: c_int,
            a6: c_int,
            a7: c_int,
            ...
        ) -> c_double;
    }
}

// what the host's own lookup gives for a name we exported
fn host_find(name: &std::ffi::CStr) -> *mut c_void {
    unsafe {
        if ape::is_windows() {
            GetProcAddress(GetModuleHandleA(c"ape-demo.dll".as_ptr()), name.as_ptr())
        } else {
            // RTLD_DEFAULT is 0 on glibc and the BSDs, -2 on macOS
            let default = if ape::is_xnu() {
                -2isize as *mut c_void
            } else {
                std::ptr::null_mut()
            };
            dlsym(default, name.as_ptr())
        }
    }
}

#[ape::host_callback]
fn posix_start(arg: *mut c_void) -> *mut c_void {
    thread_body(arg as usize);
    std::ptr::null_mut()
}

#[ape::host_callback]
fn win_start(arg: *mut c_void) -> c_uint {
    thread_body(arg as usize);
    0
}

fn host_threads(n: usize) {
    for batch in 0..n / 8 {
        if ape::is_windows() {
            let mut hs = [std::ptr::null_mut(); 8];
            for (i, h) in hs.iter_mut().enumerate() {
                let arg = (batch * 8 + i) as *mut c_void;
                *h = unsafe {
                    CreateThread(
                        std::ptr::null_mut(),
                        0,
                        win_start::ptr(),
                        arg,
                        0,
                        std::ptr::null_mut(),
                    )
                };
                assert!(!h.is_null(), "CreateThread failed");
            }
            for h in hs {
                unsafe {
                    WaitForSingleObject(h, u32::MAX);
                    CloseHandle(h);
                }
            }
        } else {
            let mut ts = [0usize; 8];
            for (i, t) in ts.iter_mut().enumerate() {
                let arg = (batch * 8 + i) as *mut c_void;
                let rc = unsafe { pthread_create(t, std::ptr::null(), posix_start::ptr(), arg) };
                assert_eq!(rc, 0, "pthread_create failed");
            }
            for t in ts {
                unsafe { pthread_join(t, std::ptr::null_mut()) };
            }
        }
    }
}

fn main() {
    println!("host: {:?}", ape::current_os());

    let mut v = [5i32, 3, 8, 1, 9, 2, 7, 4];
    unsafe { qsort(v.as_mut_ptr().cast(), v.len(), 4, compare::ptr()) };
    assert_eq!(v, [1, 2, 3, 4, 5, 7, 8, 9]);
    println!(
        "host qsort: {v:?}, {} compares",
        COMPARES.load(Ordering::Relaxed)
    );
    // a backtrace from the comparator goes up through the host's qsort to
    // here, so it has more frames than one taken here
    let (inside, here) = (FRAMES_IN_CALLBACK.load(Ordering::Relaxed), frames());
    println!("backtrace frames: {inside} in the comparator, {here} in main");
    assert!(inside > here, "the backtrace stopped at the host's frames");
    assert_eq!(unsafe { strlen(c"hello".as_ptr()) }, 5);

    let n = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    host_threads(n);
    // the host joined every thread, but a Drop runs as the thread exits,
    // which can be a moment after the join returns on some hosts
    for _ in 0..200 {
        if DROPS.load(Ordering::Relaxed) >= n {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (bodies, drops) = (
        BODIES.load(Ordering::Relaxed),
        DROPS.load(Ordering::Relaxed),
    );
    println!("host threads: {bodies} ran Rust code, {drops} thread_local drops");
    assert_eq!(bodies, n);
    assert_eq!(drops, n);
    assert_eq!(SEEN.lock().unwrap().len(), n);

    let rc = if ape::is_windows() {
        unsafe { _access(c"/no/such/file".as_ptr(), 0) }
    } else {
        unsafe { access(c"/no/such/file".as_ptr(), 0) }
    };
    let e = ape::host::errno();
    println!("host errno after a failed access(): {e:?} (rc {rc})");
    assert_eq!(rc, -1);
    assert_eq!(e, Some(2));

    // ECONNREFUSED is 111 on Linux, 61 on macOS and 107 in the CRT
    ape::host::set_errno(111);
    let raw = ape::host::errno_raw();
    println!(
        "host errno set to ECONNREFUSED: {:?} raw {raw:?}",
        ape::host::errno()
    );
    assert_eq!(ape::host::errno(), Some(111));
    let want = match ape::current_os() {
        ape::Os::Windows => 107,
        ape::Os::Xnu => 61,
        _ => 111,
    };
    assert_eq!(raw, Some(want));

    let p = unsafe { pow(2.0, 10.0) };
    let l = unsafe { ldexp(1.5, 4) };
    println!("pow(2, 10) = {p}, ldexp(1.5, 4) = {l}");
    assert_eq!(p, 1024.0);
    assert_eq!(l, 24.0);

    // ints and doubles mixed into the variadic part, which Windows and
    // Apple Silicon each pass in their own way
    let mut buf = [0u8; 64];
    let fmt = c"%s %d %.2f %lld %c";
    let args = (buf.as_mut_ptr() as *mut c_char, buf.len(), fmt.as_ptr());
    let n = unsafe {
        if ape::is_windows() {
            ape::host_variadic!(_snprintf(
                args.0,
                args.1,
                args.2,
                c"host".as_ptr(),
                42,
                3.14159f64,
                1i64 << 40,
                'x' as c_int
            ))
        } else {
            ape::host_variadic!(snprintf(
                args.0,
                args.1,
                args.2,
                c"host".as_ptr(),
                42,
                3.14159f64,
                1i64 << 40,
                'x' as c_int
            ))
        }
    };
    let s = std::str::from_utf8(&buf[..n.max(0) as usize]).unwrap_or("?");
    println!("host snprintf: {n} bytes, {s:?}");
    assert_eq!(s, "host 42 3.14 1099511627776 x");

    check_environment();

    let exported = DEMO.export().unwrap_or_else(|e| panic!("export: {e}"));
    let found = host_find(c"ape_hello");
    assert!(!found.is_null(), "the host can't find ape_hello");
    let r = if ape::is_windows() {
        #[cfg(target_arch = "x86_64")]
        {
            let f: extern "win64" fn(c_int) -> c_int = unsafe { std::mem::transmute(found) };
            f(20)
        }
        #[cfg(not(target_arch = "x86_64"))]
        unreachable!()
    } else {
        // what the host found is the library's trampoline, which host code
        // would call directly; from here it goes through cosmo_dltramp
        assert_eq!(Some(found), exported.sym("ape_hello"));
        let f = exported.func("ape_hello").expect("ape_hello");
        let f: extern "C" fn(c_int) -> c_int = unsafe { std::mem::transmute(f) };
        f(20)
    };
    println!("host dlsym found our ape_hello, which returns {r}");
    assert_eq!(r, 41);

    // the variadic one, called the way a plugin calls it: the arguments
    // laid out in the host's convention, which the library's trampoline
    // turns into our va_list
    let mixed = if ape::is_windows() {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            plugin_view::ape_mix::win64()(
                2,
                3 as c_int,
                0.5 as c_double,
                4 as c_int,
                2.0 as c_double,
            )
        }
        #[cfg(not(target_arch = "x86_64"))]
        unreachable!()
    } else if ape::is_xnu() {
        #[cfg(target_arch = "aarch64")]
        unsafe {
            use ape::host::VarArg;
            plugin_view::ape_mix::xnu(&[
                VarArg::classify(2 as c_int),
                VarArg::classify(3 as c_int),
                VarArg::classify(0.5 as c_double),
                VarArg::classify(4 as c_int),
                VarArg::classify(2.0 as c_double),
            ])
        }
        #[cfg(not(target_arch = "aarch64"))]
        unreachable!()
    } else {
        unsafe {
            plugin_view::ape_mix::sysv()(
                2,
                3 as c_int,
                0.5 as c_double,
                4 as c_int,
                2.0 as c_double,
            )
        }
    };
    println!("our variadic ape_mix called the host's way: {mixed}");
    assert_eq!(mixed, 3.0 * 0.5 + 4.0 * 2.0);
    // and as a Rust function
    assert_eq!(unsafe { ape_mix(1, 6 as c_int, 1.5 as c_double) }, 9.0);
    // the one with eight fixed parameters: the trampoline passes the
    // ones past the registers on the stack
    let mixed8 = if ape::is_windows() {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            plugin_view::ape_mix8::win64()(
                2,
                1,
                2,
                3,
                4,
                5,
                6,
                7,
                0.25 as c_double,
                0.5 as c_double,
            )
        }
        #[cfg(not(target_arch = "x86_64"))]
        unreachable!()
    } else if ape::is_xnu() {
        #[cfg(target_arch = "aarch64")]
        unsafe {
            use ape::host::VarArg;
            plugin_view::ape_mix8::xnu(&[
                VarArg::classify(2 as c_int),
                VarArg::classify(1 as c_int),
                VarArg::classify(2 as c_int),
                VarArg::classify(3 as c_int),
                VarArg::classify(4 as c_int),
                VarArg::classify(5 as c_int),
                VarArg::classify(6 as c_int),
                VarArg::classify(7 as c_int),
                VarArg::classify(0.25 as c_double),
                VarArg::classify(0.5 as c_double),
            ])
        }
        #[cfg(not(target_arch = "aarch64"))]
        unreachable!()
    } else {
        unsafe {
            plugin_view::ape_mix8::sysv()(2, 1, 2, 3, 4, 5, 6, 7, 0.25 as c_double, 0.5 as c_double)
        }
    };
    println!("ape_mix8 with eight fixed parameters called the host's way: {mixed8}");
    assert_eq!(mixed8, 140.75);

    // the variable the host finds under its name is ours, not a copy
    let counter = host_find(c"ape_counter") as *mut c_int;
    assert!(!counter.is_null(), "the host can't find ape_counter");
    assert_eq!(counter, ape_counter.as_ptr());
    assert_eq!(unsafe { ape_counter.get() }, 7);
    unsafe { *counter = 42 };
    println!("the host writes ape_counter, we read {}", unsafe {
        ape_counter.get()
    });
    assert_eq!(unsafe { ape_counter.get() }, 42);

    let name = LIBC.load().map(|l| l.name().to_owned()).unwrap_or_default();
    println!("\nhost dlopen ok ({name})");
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn host_getenv(name: &str) -> Option<String> {
    let c = std::ffi::CString::new(name).unwrap();
    let p = unsafe { getenv(c.as_ptr()) };
    (!p.is_null()).then(|| {
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_string_lossy()
            .into_owned()
    })
}

fn win_getenv(name: &str) -> Option<String> {
    let mut buf = [0u16; 512];
    let n = unsafe { GetEnvironmentVariableW(wide(name).as_ptr(), buf.as_mut_ptr(), 512) };
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

// one environment, seen the same from both sides
fn check_environment() {
    unsafe { std::env::set_var("APE_ENV_A", "from rust") };
    if ape::is_windows() {
        assert_eq!(win_getenv("APE_ENV_A").as_deref(), Some("from rust"));
        assert_eq!(
            host_getenv("APE_ENV_A").as_deref(),
            Some("from rust"),
            "the CRT's copy"
        );
        // the CRT's _putenv_s goes through SetEnvironmentVariableW
        assert_eq!(
            unsafe { _putenv_s(c"APE_ENV_B".as_ptr(), c"from the crt".as_ptr()) },
            0
        );
        assert_eq!(std::env::var("APE_ENV_B").as_deref(), Ok("from the crt"));
        let ok = unsafe { SetEnvironmentVariableW(wide("APE_ENV_A").as_ptr(), std::ptr::null()) };
        assert_ne!(ok, 0);
        assert_eq!(std::env::var_os("APE_ENV_A"), None);
        // a path is spelled the DOS way on the host's side
        unsafe { std::env::set_var("APE_ENV_P", "/c/Windows/System32") };
        let p = win_getenv("APE_ENV_P").unwrap_or_default();
        assert!(p.eq_ignore_ascii_case("C:\\Windows\\System32"), "{p}");
        unsafe {
            SetEnvironmentVariableW(wide("APE_ENV_Q").as_ptr(), wide("C:\\Temp\\x").as_ptr())
        };
        let q = std::env::var("APE_ENV_Q").unwrap_or_default();
        assert!(q.ends_with("/Temp/x"), "{q}");
        println!("environment: rust -> crt and kernel32, crt and kernel32 -> rust, {p} <-> {q}");
    } else {
        assert_eq!(host_getenv("APE_ENV_A").as_deref(), Some("from rust"));
        assert_eq!(
            unsafe { setenv(c"APE_ENV_B".as_ptr(), c"from the host".as_ptr(), 1) },
            0
        );
        assert_eq!(std::env::var("APE_ENV_B").as_deref(), Ok("from the host"));
        assert_eq!(unsafe { unsetenv(c"APE_ENV_A".as_ptr()) }, 0);
        assert_eq!(std::env::var_os("APE_ENV_A"), None);
        println!("environment: rust -> host, host -> rust, host unsetenv seen by rust");
    }
    // std::env::vars() reads the array directly rather than through libc
    assert!(
        std::env::vars().any(|(k, _)| k == "APE_ENV_B"),
        "vars() misses APE_ENV_B"
    );
    assert!(
        !std::env::vars().any(|(k, _)| k == "APE_ENV_A"),
        "vars() still has APE_ENV_A"
    );
    unsafe { std::env::remove_var("APE_ENV_B") };
    if ape::is_windows() {
        assert_eq!(win_getenv("APE_ENV_B"), None);
    } else {
        assert_eq!(host_getenv("APE_ENV_B"), None);
    }
}
