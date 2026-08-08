//! Blocking file IO
//!
//! Every `metadata()` here goes through `struct stat`. libc.patch
//! has to redefine layout for aarch64, or the wrong offset makes `is_file()` lies
//! instead of failing.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

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

    big_file(&path);

    // Truncate back to empty.
    File::create(&path).expect("truncate").sync_all().expect("sync");
    assert_eq!(fs::metadata(&path).expect("metadata").len(), 0, "truncate");
    println!("truncated to 0 bytes");

    fs::remove_file(&path).expect("remove_file");
    assert!(!path.exists(), "file outlived remove_file");

    println!("\nfile io ok");
}

/// Offsets past the 32-bit line. The file is sparse, so only the touched
/// blocks reach the disk, but a machine that won't hand out the length at all
/// (a CI runner short on quota, most likely) gets to skip this.
fn big_file(path: &Path) {
    const GIB: u64 = 1 << 30;
    const BIG_LEN: u64 = 5 * GIB;
    const MARK: [u8; 13] = *b"past-four-gib";
    let big = path.with_extension("big");
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&big)
        .expect("create the big file");
    if let Err(e) = f.set_len(BIG_LEN) {
        println!("skipped the 4 GiB crossing, set_len({BIG_LEN}) failed: {e}");
        drop(f);
        let _ = fs::remove_file(&big);
        return;
    }
    let md = fs::metadata(&big).expect("metadata big");
    assert_eq!(md.len(), BIG_LEN, "set_len past 4 GiB reported the wrong length");

    // One write straddling the boundary, one past it, one at the end.
    let spots = [4 * GIB - 3, 4 * GIB + 4096, BIG_LEN - MARK.len() as u64];
    for spot in spots {
        let pos = f.seek(SeekFrom::Start(spot)).expect("seek past 4 GiB");
        assert_eq!(pos, spot, "seek reported an offset it did not take");
        f.write_all(&MARK).expect("write past 4 GiB");
    }
    f.sync_all().expect("sync big");

    for spot in spots {
        f.seek(SeekFrom::Start(spot)).expect("seek back");
        let mut back = [0u8; MARK.len()];
        f.read_exact(&mut back).expect("read back past 4 GiB");
        assert_eq!(back, MARK, "what came back from offset {spot} is not what went in");
        let pos = f.stream_position().expect("stream_position");
        assert_eq!(pos, spot + MARK.len() as u64, "position drifted after a big read");
    }

    // SeekFrom::End resolved against a length that doesn't fit in 32 bits.
    let pos = f.seek(SeekFrom::End(-(MARK.len() as i64))).expect("seek from end");
    assert_eq!(pos, BIG_LEN - MARK.len() as u64, "SeekFrom::End past 4 GiB");
    let mut back = [0u8; MARK.len()];
    f.read_exact(&mut back).expect("read at the end");
    assert_eq!(back, MARK, "the mark at the end came back changed");

    // Untouched space inside the file reads as zeros.
    f.seek(SeekFrom::Start(3 * GIB)).expect("seek into the hole");
    let mut hole = [0xffu8; 32];
    f.read_exact(&mut hole).expect("read the hole");
    assert_eq!(hole, [0u8; 32], "the sparse hole did not read as zeros");

    println!("4 GiB crossing ok: {} marks placed and read back in a {BIG_LEN}-byte file", spots.len());
    drop(f);
    fs::remove_file(&big).expect("remove the big file");
}

fn scratch_path() -> PathBuf {
    std::env::temp_dir().join(format!("rust-ape-file-io-{}.txt", std::process::id()))
}
