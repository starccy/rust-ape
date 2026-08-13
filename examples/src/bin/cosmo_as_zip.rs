//! every cosmo's APE binary is also a valid zip archive, so you can append files
//! to it and read them back from the same binary directly. 
//!
//! Those files are located in a virtual directory called `/zip/`.
//! 
//! This example use `zip` crate just for append the assets to the binary.
//! Read the assets themselves does not require any external crate, just use `std::fs`.

use std::fs;
use std::io::Write;
use std::process::Command;

const ZIPOS_PATH: &str = "/zip/some_file.txt";
const ASSET_TEXT: &str = "hello from the zip store";
const CHILD_VAR: &str = "RUST_APE_ZIP_CHILD";

fn main() {
    if std::env::var_os(CHILD_VAR).is_some() {
        child();
    } else if let Ok(text) = fs::read_to_string(ZIPOS_PATH) {
        // someone appended an asset by hand, just show it
        println!("read {ZIPOS_PATH}: {:?}", text.trim());
        list_zip();
        println!("\ncosmo_as_zip ok");
    } else {
        parent();
    }
}

/// Runs in the copy, which carries the asset the parent appended.
fn child() {
    let text = fs::read_to_string(ZIPOS_PATH).expect("the copy can't see its own asset");
    assert_eq!(text, ASSET_TEXT, "asset came back wrong: {text:?}");
    println!("child read {ZIPOS_PATH}: {text:?}");

    let meta = fs::metadata(ZIPOS_PATH).expect("metadata on the asset");
    assert!(meta.is_file());
    assert_eq!(meta.len(), ASSET_TEXT.len() as u64);

    list_zip();
}

fn list_zip() {
    let mut names: Vec<String> = fs::read_dir("/zip")
        .expect("read_dir /zip")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    println!("read_dir /zip: {names:?}");
}

fn parent() {
    let exe = ape::program_executable_name().expect("program_executable_name");
    let copy = std::env::temp_dir().join(format!("cosmo_as_zip-{}.com", std::process::id()));
    fs::copy(&exe, &copy).expect("copy self");
    println!("copied itself to {}", copy.display());

    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&copy)
        .expect("open the copy");
    let mut w = zip::ZipWriter::new_append(f).expect("the binary should parse as a zip");
    w.start_file(
        "some_file.txt",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored),
    )
    .expect("start_file");
    w.write_all(ASSET_TEXT.as_bytes()).expect("write the asset");
    w.finish().expect("finish the zip");
    println!("appended some_file.txt ({} bytes)", ASSET_TEXT.len());

    let status = Command::new(&copy)
        .env(CHILD_VAR, "1")
        .status()
        .expect("run the copy");
    let _ = fs::remove_file(&copy);
    assert!(status.success(), "the copy exited {:?}", status.code());

    println!("\ncosmo_as_zip ok");
}
