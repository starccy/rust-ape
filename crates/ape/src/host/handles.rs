//! Paths and file descriptors as a host library knows them.

use std::ffi::{CString, c_char, c_int};

use super::{cosmo_dlfd, cosmo_dlhandle, cosmo_hostpath};

/// The path to give a host library for `path`, as a C string.
///
/// A host library opens files with the host's own libc, which doesn't
/// know the zip store. For a path the zip store serves, under `/zip` or
/// one of the paths `bundle.toml` maps there, this makes a read-only copy
/// in a private temporary directory the first time it's asked for, and
/// returns the copy's path; the copy is removed when the process exits.
/// A directory is copied whole, and a shared library along with the rest
/// of its directory. Any path is spelled the way the host spells it,
/// which on Windows is `C:\Users\...` rather than `/C/Users/...`.
///
/// A host library that opens such a path itself gets the copy as well: on
/// Linux through the dlopen helper, and on Windows and macOS because the
/// file functions it imports (`open`, `fopen`, `stat`, `opendir`, `dlopen`,
/// `CreateFileW`, `FindFirstFileW`, `LoadLibraryW` and a few more) are
/// pointed at ones that map the path first. This is for the rest: a path
/// the library passes to something else, or a `C:\` spelling it needs.
pub fn host_path(path: impl AsRef<std::path::Path>) -> std::io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_ref().as_os_str().as_bytes())?;
    let mut buf = vec![0u8; 4096];
    let p = unsafe { cosmo_hostpath(c.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len()) };
    if p.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(n);
    Ok(CString::new(buf)?)
}

/// What a host library knows file descriptor `fd` as: the Win32 `HANDLE` on
/// Windows, `fd` itself elsewhere.
pub fn raw_handle(fd: i32) -> std::io::Result<isize> {
    let h = unsafe { cosmo_dlhandle(fd) };
    if h == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(h)
    }
}

/// A file descriptor for something a host library handed over, the reverse
/// of [`raw_handle`]. On Windows `handle` is a Win32 `HANDLE` to a file, pipe,
/// console or socket; elsewhere it's the descriptor itself. `flags` is the
/// access mode the handle was opened with (`libc::O_RDONLY`, `O_WRONLY` or
/// `O_RDWR`), optionally with `O_APPEND`, `O_CLOEXEC` or `O_NONBLOCK`.
///
/// # Safety
///
/// The returned descriptor owns `handle` and closes it when dropped, so
/// nothing else may close or keep using it.
pub unsafe fn fd_from_raw_handle(
    handle: isize,
    flags: c_int,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { cosmo_dlfd(handle, flags) };
    if fd == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
    }
}
