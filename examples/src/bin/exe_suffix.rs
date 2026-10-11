//! The NT .exe magic in shim/suffix.c. On Windows a bare name that
//! misses is found as name.exe by metadata, access and spawn, also when
//! a symlink points at the bare name. A name with an extension, or one
//! in a directory that doesn't exist, stays missing. Elsewhere nothing
//! is retried.

use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::process::Command;

fn access(p: &Path) -> bool {
    let c = CString::new(p.to_str().unwrap()).unwrap();
    unsafe { libc::access(c.as_ptr(), libc::F_OK) == 0 }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("child") {
        println!("child ran");
        return;
    }
    let win = ape::is_windows();
    let dir = std::env::temp_dir().join(format!("exe_suffix-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::copy(std::env::current_exe().unwrap(), dir.join("tool.exe")).unwrap();
    fs::write(dir.join("notes.txt.exe"), b"").unwrap();

    let bare = dir.join("tool");
    assert_eq!(
        fs::metadata(&bare).is_ok(),
        win,
        "metadata of the bare name"
    );
    assert_eq!(access(&bare), win, "access of the bare name");
    assert!(
        fs::metadata(dir.join("notes.txt")).is_err(),
        "a name with an extension"
    );
    assert!(
        fs::metadata(dir.join("nodir/tool")).is_err(),
        "a missing directory"
    );
    assert!(
        fs::metadata(dir.join("nodir/deeper/tool")).is_err(),
        "two missing directories"
    );
    let err = fs::metadata(dir.join("tool.exe/tool")).unwrap_err();
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOTDIR),
        "a file as the directory"
    );
    println!("metadata/access ok");

    if win {
        let out = Command::new(&bare).arg("child").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "child ran");
        println!("spawn of the bare name ok");
    }

    // needs developer mode or admin on Windows; skipped when refused
    let link = dir.join("tool-link");
    match std::os::unix::fs::symlink("tool", &link) {
        Ok(()) => {
            assert_eq!(fs::metadata(&link).is_ok(), win, "a link to the bare name");
            println!("symlink chain ok");
        }
        Err(e) => println!("symlink skipped: {e}"),
    }

    fs::remove_dir_all(&dir).unwrap();
    println!("ok");
}
