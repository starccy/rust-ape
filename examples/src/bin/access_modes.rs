//! access/faccessat amode bits, and the group list a permission check made
//! without them needs. Neither is what a Linux-coded caller compiled in:
//! cosmo publishes NT's access mask for R_OK/W_OK/X_OK on Windows, and has
//! no group list to enumerate there.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;

fn access(path: &std::path::Path, mode: libc::c_int) -> Result<(), std::io::Error> {
    let c = CString::new(path.as_os_str().as_bytes()).expect("path with NUL");
    if unsafe { libc::access(c.as_ptr(), mode) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn faccessat(path: &std::path::Path, mode: libc::c_int, flags: libc::c_int) -> Result<(), std::io::Error> {
    let c = CString::new(path.as_os_str().as_bytes()).expect("path with NUL");
    if unsafe { libc::faccessat(libc::AT_FDCWD, c.as_ptr(), mode, flags) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");

    let data = dir.path().join("data");
    fs::write(&data, b"x").expect("write");
    let marked = dir.path().join("marked");
    fs::write(&marked, b"x").expect("write");
    fs::set_permissions(&marked, fs::Permissions::from_mode(0o755)).expect("chmod");
    let exe = std::path::PathBuf::from(std::env::args_os().next().expect("argv[0]"));
    assert!(exe.components().count() > 1, "run this by path, so argv[0] names the binary");

    access(&data, libc::F_OK).expect("F_OK on an existing file");
    println!("access F_OK ok");

    access(&data, libc::R_OK).expect("R_OK on a readable file");
    access(&data, libc::W_OK).expect("W_OK on a writable file");
    access(&data, libc::R_OK | libc::W_OK).expect("R_OK|W_OK on a readable, writable file");
    println!("access R_OK/W_OK ok");

    access(&exe, libc::X_OK).expect("X_OK on this very binary");
    println!("access X_OK ok");

    // Where executability follows the name rather than the mode, "no" is a
    // correct answer -- but it has to be EACCES, not a rejected question.
    match access(&marked, libc::X_OK) {
        Ok(()) => println!("access X_OK via mode bits ok"),
        Err(e) if e.raw_os_error() == Some(libc::EACCES) => {
            println!("access X_OK via mode bits: EACCES (host reads the name, not the mode)")
        }
        Err(e) => panic!("X_OK on a file with the execute bits set: {e}"),
    }

    faccessat(&exe, libc::X_OK, 0).expect("faccessat X_OK");
    faccessat(&exe, libc::X_OK, libc::AT_EACCESS).expect("faccessat X_OK with AT_EACCESS");
    println!("faccessat X_OK ok");

    let gone = dir.path().join("gone");
    let e = access(&gone, libc::R_OK).expect_err("R_OK on a file that is not there");
    assert_eq!(e.raw_os_error(), Some(libc::ENOENT), "missing file reported as something other than ENOENT");
    println!("access ENOENT ok");

    let e = access(&data, 0x40).expect_err("an amode bit that is not R/W/X");
    assert_eq!(e.raw_os_error(), Some(libc::EINVAL), "an unknown amode bit should be EINVAL");
    println!("access EINVAL ok");

    let n = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    assert!(n >= 1, "getgroups(0, NULL) failed: {}", std::io::Error::last_os_error());
    let mut groups = vec![0 as libc::gid_t; n as usize];
    let got = unsafe { libc::getgroups(n, groups.as_mut_ptr()) };
    assert_eq!(got, n, "getgroups filled a different count than it promised: {}", std::io::Error::last_os_error());
    assert!(groups.iter().all(|&g| g != libc::gid_t::MAX), "getgroups left the list unwritten");
    println!("getgroups ok ({n} group(s))");

    println!("all access checks ok");
}
