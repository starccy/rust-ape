//! A host function that tail-calls a thunk it was handed, on Windows.
//!
//! A thunk cosmo makes for a host function (what `Library::typed`,
//! `ape::host::thunk` and C's `dlsym` give) tells our callers from the
//! host's by the return address: one inside this program means a caller
//! in our convention whose arguments it has to move. A host function that
//! tail-calls such a thunk leaves it the return address of whoever called
//! that function. So every call into host code that this program makes in
//! the host's convention is made from a stub outside the program
//! (`cosmo_dlhostcall()` in the fork): `Library::func`, `#[host_library]`
//! and the wrappers `cargo xtask build` writes for -sys crates go through
//! it, and so does libc for the thunks whose arguments it lays out itself.
//!
//! The host function here is a few bytes of code in a page of our own,
//! which is outside the program as far as the thunk can tell: it jumps to
//! its first argument with the other three. Windows on x86-64 only.

#[cfg(target_arch = "x86_64")]
use std::ffi::c_void;
#[cfg(target_arch = "x86_64")]
use std::time::Instant;

#[cfg(target_arch = "x86_64")]
use ape::host::{self, Library};

// mov %rcx,%rax; mov %rdx,%rcx; mov %r8,%rdx; mov %r9,%r8; jmp *%rax
#[cfg(target_arch = "x86_64")]
const TAIL_CALLER: [u8; 14] = [
    0x48, 0x89, 0xc8, 0x48, 0x89, 0xd1, 0x4c, 0x89, 0xc2, 0x4d, 0x89, 0xc8, 0xff, 0xe0,
];

#[cfg(target_arch = "x86_64")]
type Caller = unsafe extern "win64" fn(*mut c_void, i32, i32, i32) -> i32;
#[cfg(target_arch = "x86_64")]
type MulDiv = unsafe extern "C" fn(i32, i32, i32) -> i32;

#[cfg(target_arch = "x86_64")]
fn code_page(code: &[u8]) -> *mut c_void {
    unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert!(
            p != libc::MAP_FAILED,
            "mmap: {}",
            std::io::Error::last_os_error()
        );
        std::ptr::copy_nonoverlapping(code.as_ptr(), p as *mut u8, code.len());
        p
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn main() {
    println!("not x86-64, nothing to do here");
}

#[cfg(target_arch = "x86_64")]
fn main() {
    if !ape::is_windows() {
        println!("not Windows, nothing to do here");
        return;
    }
    let k32 = Library::open("kernel32.dll").expect("kernel32");
    // a thunk: ours to call in our convention, the host's to call in its own
    let muldiv: MulDiv = k32.typed("MulDiv").expect("MulDiv");
    assert_eq!(unsafe { muldiv(6, 7, 2) }, 21);
    let muldiv_ptr = muldiv as usize as *mut c_void;

    let caller = code_page(&TAIL_CALLER);

    // the host function called through the stub: the thunk sees the stub's
    // return address and goes straight to MulDiv
    let via_stub: Caller = unsafe { std::mem::transmute(host::direct(caller, Some(4))) };
    let r = unsafe { via_stub(muldiv_ptr, 6, 7, 2) };
    println!("host tail-calls the thunk, called through the stub: {r}");
    assert_eq!(r, 21);

    // called straight from this program, the thunk takes us for a caller in
    // our convention and moves the arguments (what it gets is whatever was
    // in those registers)
    let raw: Caller = unsafe { std::mem::transmute(caller) };
    let r = unsafe { raw(muldiv_ptr, 6, 7, 2) };
    println!("host tail-calls the thunk, called directly: {r}");

    // the same host function reached through a thunk of its own whose
    // arguments libc lays out (a floating point one makes it that kind):
    // libc makes that call from the stub too
    type CallerF = unsafe extern "C" fn(*mut c_void, i32, i32, i32, f64) -> i32;
    let through_thunk: CallerF = unsafe { host::thunk(caller) }.expect("thunk");
    let r = unsafe { through_thunk(muldiv_ptr, 6, 7, 2, 1.5) };
    println!("host tail-calls the thunk, called through a thunk with a float: {r}");
    assert_eq!(r, 21);

    // Library::func hands out the stub; what it costs against the direct call
    let direct: unsafe extern "win64" fn(i32, i32, i32) -> i32 =
        unsafe { std::mem::transmute(k32.sym("MulDiv").unwrap()) };
    let stub: unsafe extern "win64" fn(i32, i32, i32) -> i32 =
        unsafe { std::mem::transmute(k32.func_n("MulDiv", 3).unwrap()) };
    assert_eq!(unsafe { stub(6, 7, 2) }, 21);
    let n = 5_000_000;
    let t = Instant::now();
    let mut acc = 0i64;
    for i in 0..n {
        acc += unsafe { direct(i, 7, 2) } as i64;
    }
    let d = t.elapsed();
    let t = Instant::now();
    for i in 0..n {
        acc -= unsafe { stub(i, 7, 2) } as i64;
    }
    let s = t.elapsed();
    assert_eq!(acc, 0);
    println!(
        "{n} MulDiv calls: direct {:.1} ns each, through the stub {:.1} ns each",
        d.as_nanos() as f64 / n as f64,
        s.as_nanos() as f64 / n as f64
    );

    println!("\nhost tailcall ok");
}
