//! Blocking file IO
//!
//! Every `metadata()` here goes through `struct stat`. libc.patch
//! has to redefine layout for aarch64, or the wrong offset makes `is_file()` lies
//! instead of failing.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

const FIRST: &str = "cosmopolitan\n";
const SECOND: &str = "actually portable\n";

fn main() {
    let path = scratch_path();
    println!("scratch file: {}", path.display());

    // Create and write.
    let mut f = File::create(&path).expect("create");
    f.write_all(FIRST.as_bytes()).expect("write");
    f.sync_all().expect("sync_all");
    drop(f);

    let md = fs::metadata(&path).expect("metadata");
    assert!(md.is_file(), "should be a regular file");
    assert!(!md.is_dir(), "should not be a directory");
    assert_eq!(md.len(), FIRST.len() as u64, "length after write");
    println!("wrote {} bytes", md.len());

    // Read the whole thing back.
    let read = fs::read_to_string(&path).expect("read_to_string");
    assert_eq!(read, FIRST, "content differs from what we wrote");

    // Append.
    let mut f = OpenOptions::new().append(true).open(&path).expect("open append");
    f.write_all(SECOND.as_bytes()).expect("append");
    drop(f);

    let read = fs::read_to_string(&path).expect("read after append");
    assert_eq!(read, format!("{FIRST}{SECOND}"), "append landed wrong");
    println!("appended, now {} bytes", read.len());

    // Seek and read a slice out of the middle.
    let mut f = File::open(&path).expect("open for seek");
    f.seek(SeekFrom::Start(FIRST.len() as u64)).expect("seek");
    let mut tail = String::new();
    f.read_to_string(&mut tail).expect("read tail");
    assert_eq!(tail, SECOND, "seek landed on the wrong offset");

    let pos = f.seek(SeekFrom::End(0)).expect("seek end");
    assert_eq!(pos, (FIRST.len() + SECOND.len()) as u64, "SeekFrom::End");
    drop(f);

    let copy = path.with_extension("copy");
    let copied = fs::copy(&path, &copy).expect("copy");
    assert_eq!(copied, read.len() as u64, "copy reported the wrong length");
    assert_eq!(fs::read_to_string(&copy).expect("read copy"), read, "copy differs");
    println!("copied {copied} bytes to {}", copy.display());
    fs::remove_file(&copy).expect("remove copy");

    // Truncate back to empty.
    File::create(&path).expect("truncate").sync_all().expect("sync");
    assert_eq!(fs::metadata(&path).expect("metadata").len(), 0, "truncate");
    println!("truncated to 0 bytes");

    fs::remove_file(&path).expect("remove_file");
    assert!(!path.exists(), "file outlived remove_file");

    println!("\nfile io ok");
}

fn scratch_path() -> PathBuf {
    std::env::temp_dir().join(format!("rust-ape-file-io-{}.txt", std::process::id()))
}
