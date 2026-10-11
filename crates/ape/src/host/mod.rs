//! Calling C libraries that live on the host, found at runtime.
//!
//! An APE binary is static, so a `-sys` crate that links a system library at
//! build time can't work. What works is `cosmo_dlopen()`, which loads the
//! library with the host's own loader and hands back its functions. This
//! module wraps that, and [`host_library`](crate::host_library) turns a
//! list of C declarations into Rust functions that load their library and
//! look themselves up on first use.
//!
//! The ABI is the one thing that differs by host. On Linux, macOS and the
//! BSDs a host function is called like any `extern "C"` function, through a
//! small trampoline cosmo generates. On Windows it takes Microsoft's x64
//! convention, so the generated code calls it as `extern "win64"`, which gets
//! floating point, struct and variadic arguments right. That is also why a
//! callback handed to a host library has to exist in both conventions, see
//! [`host_callback`](crate::host_callback).
//!
//! Callbacks may run on any thread. A thread the host library started gets
//! cosmo's thread state the first time it runs our code, so the whole of
//! libc and std work there, thread locals and their destructors included.

pub mod ffi;
pub mod layout;

mod callback;
mod errno;
mod exports;
mod handles;
mod library;
mod macros;
mod vararg;

pub use self::callback::{Callback, HostFn, callback, callback_raw, direct, thunk, thunk_raw};
pub use self::errno::{errno, errno_location, errno_raw, set_errno, set_errno_raw};
pub use self::exports::{ExportItem, Exports, Shared, export, export_items};
pub use self::handles::{fd_from_raw_handle, host_path, raw_handle};
pub use self::library::{LazyLibrary, Library};
pub use self::vararg::{Kind, VaArg, VaList, VarArg, VarRet, XnuKind, xnu_min};
#[cfg(target_arch = "aarch64")]
pub use self::vararg::{XnuArgs, xnu_bits, xnu_packs, xnu_pcall, xnu_vcall};

use std::ffi::{c_char, c_int, c_void};

use self::exports::Export;

unsafe extern "C" {
    fn cosmo_dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn cosmo_dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn cosmo_dltramp(func: *mut c_void) -> *mut c_void;
    fn cosmo_dlhostcall(func: *mut c_void, nargs: c_int) -> *mut c_void;
    fn cosmo_dlclose(handle: *mut c_void) -> c_int;
    fn cosmo_dlerror() -> *mut c_char;
    fn cosmo_dlerrno() -> *mut c_int;
    fn cosmo_dlgeterrno() -> c_int;
    fn cosmo_dlseterrno(e: c_int);
    fn cosmo_dlhandle(fd: c_int) -> isize;
    fn cosmo_dlfd(handle: isize, flags: c_int) -> c_int;
    fn cosmo_hostlink(name: *const c_char) -> *mut c_void;
    fn cosmo_dlexport(name: *const c_char, syms: *const Export, n: usize) -> *mut c_void;
    #[cfg(target_arch = "aarch64")]
    fn cosmo_envsync();
    fn __hostlink_thunk(f: *mut c_void, desc: u64) -> *mut c_void;
    fn __hostlink_callback(f: *mut c_void, desc: u64) -> *mut c_void;
}

/// Brings `std::env` up to date with variables host code set.
///
/// Host libraries and this program share one environment. On Linux and
/// Windows a change either side makes is seen by the other at once. On
/// macOS a variable the host adds (rather than changes or removes) shows
/// up the next time our side reads or writes the environment through
/// libc, which `std::env::var` and `set_var` do and `std::env::vars`
/// doesn't. Functions declared with [`host_library`](crate::host_library)
/// do this after every call, and [`host_callback`](crate::host_callback)
/// functions when they're entered; code calling host functions through
/// raw pointers can call it itself.
pub fn sync_env() {
    #[cfg(target_arch = "aarch64")]
    if crate::is_xnu() {
        unsafe { cosmo_envsync() }
    }
}

#[doc(hidden)]
#[inline(always)]
pub fn after_host_call() {
    #[cfg(target_arch = "aarch64")]
    sync_env();
}

unsafe extern "C" {
    fn cosmo_hostpath(path: *const c_char, buf: *mut c_char, size: usize) -> *mut c_char;
}
