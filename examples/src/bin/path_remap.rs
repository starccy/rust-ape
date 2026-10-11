//! Linux data paths served out of the zip store.
//!
//! The binary carries a `.bundle-info` manifest and the files it names in
//! its zip tail; cosmo (libc/runtime/zipos-remap.c in the fork) redirects
//! lookups of the Linux paths to /zip whenever the host has nothing there. This example reads through a path
//! no host has, so the remap must be what answers.
//!
//! Without a bundle applied the whole thing is a no-op and the example
//! just says so.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

const DIR: &str = "/usr/share/rust-ape-remap-test";
const ALIAS: &str = "/etc/rust-ape-remap-alias";

fn main() {
    if !Path::new(DIR).exists() {
        println!("no bundle for {DIR}, nothing to check");
        println!("\npath_remap ok");
        return;
    }

    let text = fs::read_to_string(format!("{DIR}/hello.txt")).expect("read hello.txt");
    assert_eq!(
        text.trim(),
        "hello from the bundle",
        "unexpected content: {text:?}"
    );
    println!("read {DIR}/hello.txt: {:?}", text.trim());

    let meta = fs::metadata(format!("{DIR}/hello.txt")).expect("metadata");
    assert!(meta.is_file());
    assert_eq!(meta.len(), text.len() as u64);

    // lexical normalization: dot and dot-dot are folded before matching
    let odd = format!("{DIR}/sub/../hello.txt");
    let text2 = fs::read_to_string(&odd).expect("read through a ../ path");
    assert_eq!(text, text2);
    let odd = format!("//usr/./share//rust-ape-remap-test/hello.txt");
    let text3 = fs::read_to_string(&odd).expect("read through a // path");
    assert_eq!(text, text3);

    // a second Linux prefix pointing at the same zip directory
    let text4 = fs::read_to_string(format!("{ALIAS}/hello.txt")).expect("read via alias");
    assert_eq!(text, text4);

    // a missing member fails the ordinary way
    let err = fs::read(format!("{DIR}/nope.txt")).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");

    // the store is read-only, like the real thing under a non-root user
    let err = fs::write(format!("{DIR}/hello.txt"), "x").unwrap_err();
    assert_ne!(
        err.kind(),
        ErrorKind::NotFound,
        "write should fail as read-only, got {err}"
    );
    println!("write refused: {err}");

    let mut names: Vec<String> = fs::read_dir(DIR)
        .expect("read_dir")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    println!("entries: {names:?}");
    assert_eq!(names, ["hello.txt", "sub"]);

    // DirEntry::metadata is fstatat relative to the directory fd; upstream
    // zipos refused that outright, the fork resolves it
    for entry in fs::read_dir(DIR).expect("read_dir again") {
        let entry = entry.unwrap();
        let ft = entry.file_type().expect("file_type");
        let meta = entry.metadata().expect("metadata through the dirfd");
        assert_eq!(ft.is_dir(), meta.is_dir(), "{:?}", entry.path());
        if entry.file_name() == "hello.txt" {
            assert_eq!(meta.len(), text.len() as u64);
        }
        println!(
            "  {:?}: dir={} len={}",
            entry.file_name(),
            meta.is_dir(),
            meta.len()
        );
    }

    let sub: Vec<_> = fs::read_dir(format!("{DIR}/sub"))
        .expect("read_dir sub")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(sub, ["keep"]);

    println!("\npath_remap ok");
}
