//! A UNC cwd (`\\server\share`) survives the trip through unix path code.
//! Windows only in substance; elsewhere there is no UNC spelling and
//! every check passes trivially.
//!
//! Reached through a directory symlink (`C:\shared` pointing at the
//! share), canonicalize(".") resolves the cwd to //server/share. Unix
//! path normalization then collapses the leading "//" to "/", and that
//! /server/share spelling must still name the share for write, stat and
//! removal, never a nested copy resolved relative to the cwd.

use std::fs;
use std::path::{Component, Path, PathBuf};

fn collapse(p: &Path) -> PathBuf {
    let mut r = PathBuf::new();
    for c in p.components() {
        match c {
            Component::RootDir => r.push("/"),
            Component::Normal(x) => r.push(x),
            Component::ParentDir => {
                r.pop();
            }
            Component::CurDir | Component::Prefix(_) => {}
        }
    }
    r
}

fn main() {
    // Reach the UNC spelling when the cwd is a symlink onto a share.
    let cwd = fs::canonicalize(".").expect("canonicalize cwd");
    std::env::set_current_dir(&cwd).expect("chdir");
    let cwd = std::env::current_dir().expect("current_dir");
    println!("cwd: {}", cwd.display());

    let name = format!("unc_cwd-{}.txt", std::process::id());
    let collapsed = collapse(&cwd.join(&name));
    println!("collapsed: {}", collapsed.display());
    assert!(
        collapsed.parent().unwrap().is_dir(),
        "collapsed cwd is not a directory"
    );

    fs::write(&collapsed, b"unc").expect("write through collapsed path");

    // The real test: the file is in the cwd, not in some nested copy of it.
    let seen = fs::read_dir(".")
        .expect("read_dir")
        .flatten()
        .any(|e| e.file_name().to_string_lossy() == name);
    // Where the collapsed path used to land: the cwd with its own
    // components repeated underneath it.
    let stray = collapsed
        .strip_prefix("/")
        .map(|rel| Path::new(rel).exists())
        .unwrap_or(false);
    let _ = fs::remove_file(&name);
    assert!(seen, "file written via the collapsed path is not in the cwd");
    assert!(!stray, "the collapsed path was resolved relative to the cwd");

    // The collapsed form also resolves for metadata and removal.
    fs::write(&collapsed, b"unc").expect("rewrite");
    assert_eq!(fs::metadata(&collapsed).expect("stat").len(), 3);
    fs::remove_file(&collapsed).expect("remove via collapsed path");
    assert!(!Path::new(&name).exists());
    println!("ok");
}
