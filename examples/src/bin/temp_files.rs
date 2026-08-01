//! Temporary files, through std and through the `tempfile` crate.
//!
//! `std::env::temp_dir()` returns `/tmp` unless TMPDIR says otherwise, because
//! that's what a Linux target compiles to.
//!
//! The good news is cosmo maps that path per host, so on Windows it lands in
//! `%LOCALAPPDATA%\Temp`. That's why `tempfile` works out of the box (mostly):
//! the anonymous variant still fails there, see the bottom of this file.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};

fn main() {
    let dir = std::env::temp_dir();
    println!("temp_dir: {}", dir.display());
    assert!(dir.is_dir(), "temp directory is not a directory");

    // A file we name and clean up ourselves.
    let ours = dir.join(format!("rust-ape-temp-{}.txt", std::process::id()));
    fs::write(&ours, b"by hand").expect("write");
    assert_eq!(fs::read_to_string(&ours).expect("read"), "by hand");
    fs::remove_file(&ours).expect("remove");
    println!("hand-rolled temp file ok");

    // tempfile: a named file that deletes itself on drop.
    let mut f = tempfile::NamedTempFile::new().expect("NamedTempFile::new");
    let path = f.path().to_owned();
    println!("NamedTempFile: {}", path.display());
    f.write_all(b"tempfile works").expect("write");
    f.seek(SeekFrom::Start(0)).expect("seek");
    let mut back = String::new();
    f.read_to_string(&mut back).expect("read");
    assert_eq!(back, "tempfile works", "content differs from what we wrote");
    drop(f);
    assert!(!path.exists(), "NamedTempFile outlived its drop");

    // tempfile: a whole directory, likewise.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_owned();
    println!("tempdir: {}", path.display());
    fs::write(path.join("inside.txt"), b"y").expect("write inside tempdir");
    fs::create_dir(path.join("nested")).expect("mkdir inside tempdir");
    drop(dir);
    assert!(!path.exists(), "tempdir outlived its drop");

    // The anonymous variant is the one that breaks. tempfile opens with
    // O_TMPFILE and falls back only on EOPNOTSUPP/EISDIR/ENOENT; cosmo returns
    // Windows' raw 267 rather than EISDIR, so the fallback never fires.
    // Assert instead of skipping, so we notice if either side changes.
    // Of course we could patch tempfile as well, but next to the load-bearing ones in
    // patches/ that doesn't seem worth it.
    match tempfile::tempfile() {
        Ok(mut f) => {
            f.write_all(b"anonymous").expect("write");
            f.seek(SeekFrom::Start(0)).expect("seek");
            let mut back = String::new();
            f.read_to_string(&mut back).expect("read");
            assert_eq!(back, "anonymous", "anonymous temp file lost its content");
            println!("anonymous temp file ok");
        }
        Err(e) => {
            assert!(
                ape::is_windows(),
                "anonymous tempfile failed on {:?}, which was only expected on Windows: {e}",
                ape::current_os()
            );
            println!("anonymous temp file: unsupported here ({e})");
        }
    }

    println!("\ntemp files ok");
}
