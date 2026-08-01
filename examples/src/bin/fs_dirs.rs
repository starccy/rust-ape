//! Directory trees: create nested dirs, walk them, rename, then clean up.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let root = scratch_dir();
    let _ = fs::remove_dir_all(&root); // in case a previous run died mid-way
    println!("scratch tree: {}", root.display());

    // Build a small tree.
    fs::create_dir_all(root.join("a/b/c")).expect("create_dir_all");
    fs::write(root.join("top.txt"), "top\n").expect("write top");
    fs::write(root.join("a/mid.txt"), "mid\n").expect("write mid");
    fs::write(root.join("a/b/c/leaf.txt"), "leaf\n").expect("write leaf");

    // One level: read_dir plus the file type it hands back.
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(&root).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let kind = if entry.file_type().expect("file_type").is_dir() { "dir " } else { "file" };
        println!("  {kind} {}", entry.file_name().to_string_lossy());
        names.insert(entry.file_name().to_string_lossy().into_owned());
    }
    assert_eq!(
        names,
        BTreeSet::from(["a".to_owned(), "top.txt".to_owned()]),
        "read_dir listed the wrong entries"
    );

    // Recursive walk.
    let mut files = walk(&root);
    files.sort();
    println!("walked {} files", files.len());
    assert_eq!(files.len(), 3, "expected 3 files in the tree");

    // Rename a directory, and confirm the thing inside came along.
    fs::rename(root.join("a/b"), root.join("a/renamed")).expect("rename");
    assert!(!root.join("a/b").exists(), "old directory name still there");
    let leaf = fs::read_to_string(root.join("a/renamed/c/leaf.txt")).expect("read after rename");
    assert_eq!(leaf, "leaf\n", "file content changed across a rename");
    println!("renamed a/b -> a/renamed");

    fs::remove_dir_all(&root).expect("remove_dir_all");
    assert!(!root.exists(), "tree outlived remove_dir_all");

    println!("\nfs dirs ok");
}

/// Every file under `dir`, depth first.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if entry.file_type().expect("file_type").is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("rust-ape-fs-dirs-{}", std::process::id()))
}
