//! Directory trees: create nested dirs, walk them, rename, then clean up.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const ODD_NAMES: [&str; 4] = [
    "中文文件名.txt",
    "emoji-🦀.txt",
    "with spaces and (parens).txt",
    "tildes~and'quotes.txt",
];

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

    // Non-ASCII names, in a directory that is itself non-ASCII.
    let odd = root.join("名字 folder");
    fs::create_dir(&odd).expect("create a non-ASCII directory");
    let mut written = BTreeSet::new();
    for name in ODD_NAMES {
        let path = odd.join(name);
        fs::write(&path, name.as_bytes()).expect("write to a non-ASCII path");
        let back = fs::read_to_string(&path).expect("read from a non-ASCII path");
        assert_eq!(back, name, "content differs at {name:?}");
        let md = fs::metadata(&path).expect("metadata on a non-ASCII path");
        assert_eq!(md.len(), name.len() as u64, "length differs at {name:?}");
        written.insert(name.to_owned());
    }
    let listed: BTreeSet<String> = fs::read_dir(&odd)
        .expect("read_dir on a non-ASCII directory")
        .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(listed, written, "read_dir handed back names we never created");
    println!("{} non-ASCII names round-tripped through read_dir", ODD_NAMES.len());

    // Rename between two non-ASCII names, then remove by the new one.
    let from = odd.join(ODD_NAMES[0]);
    let to = odd.join("改名了-🦀.txt");
    fs::rename(&from, &to).expect("rename a non-ASCII path");
    assert!(!from.exists(), "old non-ASCII name still there");
    assert_eq!(fs::read_to_string(&to).expect("read after rename"), ODD_NAMES[0]);
    fs::remove_file(&to).expect("remove a non-ASCII path");
    println!("renamed and removed {:?} -> {:?}", ODD_NAMES[0], "改名了-🦀.txt");

    // Long paths, reported rather than asserted. Windows' MAX_PATH is 260 and
    // whether cosmo's conversion opts into the extended form is its business,
    // not ours; what's worth having is a record of where the wall sits.
    let mut deep = root.join("deep");
    fs::create_dir(&deep).expect("create the deep root");
    let mut levels = 0;
    while levels < 24 {
        let next = deep.join("0123456789abcdef0123456789abcde");
        if fs::create_dir(&next).is_err() {
            break;
        }
        deep = next;
        levels += 1;
    }
    let chars = deep.as_os_str().len();
    let leaf = deep.join("leaf.txt");
    match fs::write(&leaf, b"deep") {
        Ok(()) => {
            assert_eq!(fs::read(&leaf).expect("read the deep leaf"), b"deep");
            println!("deepest path that round-tripped: {chars} chars over {levels} levels");
        }
        Err(e) => println!("nesting stopped at {chars} chars ({levels} levels), writing there failed: {e}"),
    }

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
