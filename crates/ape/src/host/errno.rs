//! The host libc's `errno`, which is not ours.

use super::{cosmo_dlerrno, cosmo_dlgeterrno, cosmo_dlseterrno};

/// Where the host libc keeps `errno` for this thread, which is what a host
/// library sets when it fails. Rust's `std::io::Error::last_os_error()` reads
/// ours instead. On Windows this is the Universal CRT's errno.
pub fn errno_location() -> Option<*mut i32> {
    let p = unsafe { cosmo_dlerrno() };
    (!p.is_null()).then_some(p)
}

/// The host libc's `errno` for this thread, as the host numbers it: Linux
/// values on Linux, XNU's on macOS, the Universal CRT's on Windows. For a
/// value to compare with the `libc` crate's constants see
/// [`errno`].
pub fn errno_raw() -> Option<i32> {
    errno_location().map(|p| unsafe { *p })
}

/// Sets the host libc's `errno` for this thread, in the host's numbering.
pub fn set_errno_raw(value: i32) {
    if let Some(p) = errno_location() {
        unsafe { *p = value }
    }
}

/// The host libc's `errno` for this thread, translated to the Linux
/// numbering that `std::io::Error::raw_os_error` and the `libc` crate use
/// here, so `Some(libc::ENOENT)` means the same thing on every host.
pub fn errno() -> Option<i32> {
    errno_location()?;
    Some(unsafe { cosmo_dlgeterrno() })
}

/// Sets the host libc's `errno` for this thread to `value`, given in the
/// Linux numbering [`errno`] returns.
pub fn set_errno(value: i32) {
    unsafe { cosmo_dlseterrno(value) }
}
