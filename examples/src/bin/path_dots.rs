//! "." and ".." resolved the way the Linux kernel does it, and ENOENT
//! told apart from ENOTDIR. What comes before a "." or ".." has to be a
//! directory that exists, ".." after a symlink climbs out of the link's
//! target, and getcwd() after chdir() through a link names the target.
//! NT answers "missing directory" and "file in the middle" with the same
//! error, so the fork asks again. The last part changes the tree under
//! one path back and forth, since the fork remembers where earlier
//! lookups broke off. Error messages read the same as on Linux.

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

fn errno(p: &str) -> Option<i32> {
    fs::metadata(p)
        .err()
        .map(|e| e.raw_os_error().unwrap_or(-1))
}

fn ino(p: &str) -> u64 {
    fs::metadata(p).unwrap().ino()
}

fn main() {
    let dir = std::env::temp_dir().join(format!("path_dots-{}", std::process::id()));
    fs::create_dir_all(dir.join("d/sub")).unwrap();
    std::env::set_current_dir(&dir).unwrap();
    fs::write("file", b"").unwrap();
    let links = std::os::unix::fs::symlink("d/sub", "l").is_ok()
        && std::os::unix::fs::symlink("file", "lf").is_ok()
        && std::os::unix::fs::symlink("nope", "dangling").is_ok();

    for p in [
        "file/.",
        "file/..",
        "file/../file",
        "file/./",
        "d/sub/../../file/..",
    ] {
        assert_eq!(errno(p), Some(libc::ENOTDIR), "{p}");
    }
    for p in ["nope/..", "nope/../file", "nope/./x", "d/nope/../sub"] {
        assert_eq!(errno(p), Some(libc::ENOENT), "{p}");
    }
    let (d, sub, base, file) = (ino("d"), ino("d/sub"), ino("."), ino("file"));
    for (p, want) in [
        ("d/./sub", sub),
        ("d/sub/..", d),
        ("d/sub/../..", base),
        ("./d/../file", file),
        ("d//sub/..", d),
        ("/..", ino("/")),
    ] {
        assert_eq!(ino(p), want, "{p}");
    }
    let err = fs::create_dir("file/../nd").unwrap_err();
    assert_eq!(err.raw_os_error(), Some(libc::ENOTDIR));
    // the text is Linux's too; NT's last error used to trail it
    let _ = fs::read("file");
    assert_eq!(err.to_string(), "Not a directory (os error 20)");
    assert_eq!(
        fs::create_dir("nope/../nd").unwrap_err().kind(),
        ErrorKind::NotFound
    );
    fs::create_dir("d/../nd").unwrap();
    fs::remove_dir("nd").unwrap();
    println!("dots ok");

    if links {
        for (p, want) in [
            ("l/..", d),
            ("l/../sub", sub),
            ("l/.", sub),
            ("l/./..", d),
            ("d/../l/..", d),
        ] {
            assert_eq!(ino(p), want, "{p}");
        }
        assert_eq!(errno("lf/."), Some(libc::ENOTDIR));
        assert_eq!(errno("dangling/.."), Some(libc::ENOENT));
        std::env::set_current_dir("l").unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert!(
            cwd.ends_with("d/sub"),
            "getcwd after chdir through a link: {}",
            cwd.display()
        );
        assert_eq!(ino(".."), d);
        std::env::set_current_dir(&dir).unwrap();
        println!("symlinks ok");
    } else {
        println!("symlinks skipped");
    }

    let check = |label: &str, want: i32| {
        for p in ["A/b/c/x", "A/b/c/y"] {
            assert_eq!(errno(p), Some(want), "{label}: {p}");
        }
    };
    for _ in 0..2 {
        check("A missing", libc::ENOENT);
        fs::write("A", b"").unwrap();
        check("A a file", libc::ENOTDIR);
        fs::remove_file("A").unwrap();
        fs::create_dir("A").unwrap();
        fs::write("A/b", b"").unwrap();
        check("A/b a file", libc::ENOTDIR);
        fs::remove_file("A/b").unwrap();
        fs::create_dir("A/b").unwrap();
        check("A/b/c missing", libc::ENOENT);
        fs::remove_dir_all("A").unwrap();
        check("all gone", libc::ENOENT);
        fs::write("A", b"").unwrap();
        check("A a file again", libc::ENOTDIR);
        fs::remove_file("A").unwrap();
    }
    assert!(!Path::new("A").exists());
    println!("ENOENT/ENOTDIR ok");

    std::env::set_current_dir(std::env::temp_dir()).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    println!("ok");
}
