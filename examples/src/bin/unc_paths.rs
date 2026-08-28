//! Path conversion on a UNC share, as a matrix: every case reports on its
//! own and the process fails at the end if any did.
//!
//! Run with the cwd on a share (`cd \\server\share` on NT); anywhere else
//! the UNC cases are skipped and only the generic ones run. An optional
//! `RUST_APE_UNC_LINK` names a directory symlink that points at a share,
//! for the symlink cases.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

struct Report {
    failed: Vec<String>,
}

impl Report {
    fn check(&mut self, name: &str, r: Result<String, String>) {
        match r {
            Ok(info) => println!("PASS {name}: {info}"),
            Err(e) => {
                println!("FAIL {name}: {e}");
                self.failed.push(name.to_string());
            }
        }
    }
}

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

fn e<T: std::fmt::Debug>(what: &str, r: Result<T, std::io::Error>) -> Result<T, String> {
    r.map_err(|err| format!("{what}: {err}"))
}

fn same_file(a: &Path, b: &Path) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;
    let ma = e(&format!("stat {}", a.display()), fs::metadata(a))?;
    let mb = e(&format!("stat {}", b.display()), fs::metadata(b))?;
    Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
}

fn same_path_text(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

fn main() {
    if std::env::var_os("RUST_APE_UNC_CHILD").is_some() {
        // The child half of child-inherits-cwd: print the cwd and leave.
        println!("{}", std::env::current_dir().expect("cwd").display());
        return;
    }
    let mut rep = Report { failed: Vec::new() };
    let cwd = fs::canonicalize(".").and_then(|c| {
        std::env::set_current_dir(&c)?;
        std::env::current_dir()
    });
    let cwd = cwd.expect("cwd");
    let cwd_s = cwd.to_string_lossy().into_owned();
    println!("cwd: {cwd_s}");
    let unc = cwd_s.starts_with("//");
    let pid = std::process::id();
    let base = cwd.join(format!("unc_paths-{pid}"));
    fs::create_dir(&base).expect("mkdir base");

    // Generic cases first, they run everywhere.
    let exe = PathBuf::from(std::env::args_os().next().unwrap());
    // A relative argv[0] (Linux) or a win32-spelled one (NT under a UNC
    // cwd, which unix Path also calls relative) is anchored on the cwd.
    let exe_abs = if exe.is_absolute() { exe.clone() } else { cwd.join(&exe) };
    rep.check("argv0-exists", (|| {
        let m = e("stat argv[0]", fs::metadata(&exe))?;
        Ok(format!("{} ({} bytes, absolute={})", exe.display(), m.len(), exe.is_absolute()))
    })());
    rep.check("argv0-joined-on-cwd", (|| {
        // A win32-spelled argv[0] is "relative" to unix Path and gets joined.
        let j = cwd.join(&exe);
        e("stat cwd.join(argv[0])", fs::metadata(&j))?;
        Ok(j.display().to_string())
    })());
    rep.check("current-exe", (|| {
        let p = e("current_exe", std::env::current_exe())?;
        e("stat current_exe", fs::metadata(&p))?;
        Ok(p.display().to_string())
    })());
    rep.check("unicode-name", (|| {
        let p = base.join("文件-ümlaut.txt");
        e("write", fs::write(&p, b"u"))?;
        let seen = e("read_dir", fs::read_dir(&base))?
            .flatten()
            .any(|d| d.path() == p);
        if !seen {
            return Err("not listed under its own name".into());
        }
        e("remove", fs::remove_file(&p))?;
        Ok("round-trips".into())
    })());
    rep.check("dotdot-normalizes", (|| {
        let sub = base.join("sub");
        e("mkdir sub", fs::create_dir(&sub))?;
        let marker = base.join("marker");
        e("write marker", fs::write(&marker, b"m"))?;
        let via = sub.join("..").join("marker");
        e("stat via ..", fs::metadata(&via))?;
        if !same_file(&via, &marker)? {
            return Err("sub/../marker is not marker".into());
        }
        Ok("sub/../marker is marker".into())
    })());
    rep.check("long-path", (|| {
        let mut p = base.clone();
        for i in 0..12 {
            p.push(format!("segment-{i:02}-abcdefghijklmnopqrstuvwxyz"));
        }
        e("create_dir_all deep", fs::create_dir_all(&p))?;
        let f = p.join("leaf.txt");
        e("write deep", fs::write(&f, b"deep"))?;
        let back = e("read deep", fs::read(&f))?;
        if back != b"deep" {
            return Err("content mismatch".into());
        }
        Ok(format!("{} chars", f.as_os_str().len()))
    })());
    rep.check("remove-dir-all", (|| {
        let t = base.join("tree");
        e("mkdir", fs::create_dir_all(t.join("a/b/c")))?;
        e("write", fs::write(t.join("a/b/c/f"), b"f"))?;
        e("write", fs::write(t.join("a/g"), b"g"))?;
        e("remove_dir_all", fs::remove_dir_all(&t))?;
        if t.exists() {
            return Err("still there".into());
        }
        Ok("gone".into())
    })());
    rep.check("rename", (|| {
        let a = base.join("ra");
        let b = base.join("rb");
        e("write", fs::write(&a, b"r"))?;
        e("rename", fs::rename(&a, &b))?;
        if a.exists() || !b.exists() {
            return Err("rename did not move".into());
        }
        Ok("moved".into())
    })());
    rep.check("child-inherits-cwd", (|| {
        let out = e("spawn", Command::new(&exe_abs).env("RUST_APE_UNC_CHILD", "1").current_dir(&base).output())?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() {
            return Err(format!("child failed: {s} {}", String::from_utf8_lossy(&out.stderr)));
        }
        if !same_file(Path::new(&s), &base)? {
            return Err(format!("child cwd {s} is not {}", base.display()));
        }
        Ok(format!("child saw {s}"))
    })());

    // NT only: a rooted path names the cosmos drive from every cwd, and
    // "/" lists the drive letters.
    if ape::is_windows() {
        rep.check("rooted-path-drive", (|| {
            let drive = std::env::var("COSMOSDRIVE")
                .or_else(|_| std::env::var("SYSTEMDRIVE"))
                .ok()
                .and_then(|d| d.chars().next())
                .unwrap_or('C')
                .to_ascii_uppercase();
            let m = e("stat /Windows", fs::metadata("/Windows"))?;
            if !m.is_dir() {
                return Err("/Windows is not a directory".into());
            }
            if !same_file(Path::new("/Windows"), Path::new(&format!("/{drive}/Windows")))? {
                return Err(format!("/Windows is not /{drive}/Windows"));
            }
            let names: Vec<String> = e("read_dir /", fs::read_dir("/"))?
                .flatten()
                .map(|d| d.file_name().to_string_lossy().into_owned())
                .collect();
            if !names.iter().any(|n| n.eq_ignore_ascii_case(&drive.to_string())) {
                return Err(format!("/ lists {names:?}, no drive {drive}"));
            }
            let dirs = names.iter().filter(|n| n.len() == 1).count();
            Ok(format!("/ is {drive}: and lists {dirs} drive(s)"))
        })());
        rep.check("single-letter-no-drive", (|| {
            // "/x" is drive x only when that drive exists; a letter with
            // no drive is a plain name on the cosmos drive, like "/bin".
            let drives: Vec<String> = e("read_dir /", fs::read_dir("/"))?
                .flatten()
                .map(|d| d.file_name().to_string_lossy().to_ascii_uppercase())
                .filter(|n| n.len() == 1)
                .collect();
            let letter = ('A'..='Z')
                .rev()
                .find(|c| !drives.contains(&c.to_string()))
                .ok_or("every drive letter is taken")?;
            let short = PathBuf::from(format!("/{}", letter.to_ascii_lowercase()));
            let long = PathBuf::from(format!("/C/{}", letter.to_ascii_lowercase()));
            let dir = short.join(format!("unc_paths-{pid}"));
            e("mkdir under /x", fs::create_dir_all(&dir))?;
            let r = (|| {
                if !same_file(&dir, &long.join(format!("unc_paths-{pid}")))? {
                    return Err(format!("{} is not under /C", dir.display()));
                }
                if !same_file(&short, &long)? {
                    return Err(format!("{} is not {}", short.display(), long.display()));
                }
                Ok(format!("{} is {}", short.display(), long.display()))
            })();
            let _ = fs::remove_dir_all(&dir);
            let _ = fs::remove_dir(&long);
            r
        })());
    }

    if unc {
        let root: PathBuf = cwd.components().take(3).collect(); // "//server/share"
        let server = PathBuf::from(format!("//{}", cwd.components().nth(1).unwrap().as_os_str().to_string_lossy()));
        println!("share root: {}  server: {}", root.display(), server.display());

        rep.check("share-root-stat", (|| {
            let m = e("stat share root", fs::metadata(&root))?;
            if !m.is_dir() {
                return Err("not a directory".into());
            }
            let n = e("read_dir share root", fs::read_dir(&root))?.count();
            Ok(format!("{n} entries"))
        })());
        let share_name = root.file_name().unwrap().to_string_lossy().into_owned();
        rep.check("server-parent", (|| {
            // Path::parent of the share root is //server: a directory
            // listing the server's shares.
            let p = root.parent().unwrap().to_path_buf();
            let m = e("stat //server", fs::metadata(&p))?;
            if !m.is_dir() {
                return Err("not a directory".into());
            }
            let names: Vec<String> = e("read_dir //server", fs::read_dir(&p))?
                .flatten()
                .map(|d| d.file_name().to_string_lossy().into_owned())
                .collect();
            if !names.iter().any(|n| n.eq_ignore_ascii_case(&share_name)) {
                return Err(format!("share {share_name} not in {names:?}"));
            }
            let sub = e("stat //server/share via listing", fs::metadata(p.join(&share_name)))?;
            if !sub.is_dir() {
                return Err("listed share is not a directory".into());
            }
            Ok(format!("{} lists {names:?}", p.display()))
        })());
        rep.check("server-parent-collapsed", (|| {
            let p = collapse(&root).parent().unwrap().to_path_buf();
            let n = e("read_dir /server", fs::read_dir(&p))?.count();
            Ok(format!("{} has {n} entries", p.display()))
        })());
        rep.check("collapsed-write", (|| {
            let f = collapse(&base.join("c.txt"));
            e("write", fs::write(&f, b"c"))?;
            if !base.join("c.txt").exists() {
                return Err("file not in the cwd".into());
            }
            Ok(f.display().to_string())
        })());
        rep.check("collapsed-canonicalize", (|| {
            let f = collapse(&base.join("c.txt"));
            let c = e("canonicalize", fs::canonicalize(&f))?;
            let s = c.to_string_lossy();
            if !s.starts_with("//") {
                return Err(format!("got {s}, not a //server/share path"));
            }
            Ok(s.into())
        })());
        rep.check("collapsed-chdir", (|| {
            let d = collapse(&base);
            e("chdir collapsed", std::env::set_current_dir(&d))?;
            let now = e("getcwd", std::env::current_dir())?;
            e("chdir back", std::env::set_current_dir(&cwd))?;
            if !same_file(&now, &base)? {
                return Err(format!("cwd became {}", now.display()));
            }
            Ok(now.display().to_string())
        })());
        rep.check("collapsed-from-drive-cwd", (|| {
            // The share must still be known after leaving it.
            let f = collapse(&base.join("c.txt"));
            let tmp = std::env::temp_dir();
            e("chdir temp", std::env::set_current_dir(&tmp))?;
            let r = fs::metadata(&f);
            e("chdir back", std::env::set_current_dir(&cwd))?;
            e("stat from a drive cwd", r)?;
            Ok(format!("{} resolved from {}", f.display(), tmp.display()))
        })());
        rep.check("collapsed-dotdot", (|| {
            let p = collapse(&base).join("..");
            let m = e("stat", fs::metadata(&p))?;
            if !m.is_dir() || !same_file(&p, &cwd)? {
                return Err("does not resolve to the cwd".into());
            }
            Ok("base/.. is the cwd".into())
        })());
        rep.check("share-root-dotdot", (|| {
            // ".." above the share root is the server directory (the
            // share list), and climbing further stays there, like "/.."
            // on unix. A ".." followed by a share name is that share.
            let is_server = |p: &Path| -> Result<(), String> {
                let names: Vec<String> = e(&format!("read_dir {}", p.display()), fs::read_dir(p))?
                    .flatten()
                    .map(|d| d.file_name().to_string_lossy().into_owned())
                    .collect();
                if names.iter().any(|n| n.eq_ignore_ascii_case(&share_name)) {
                    Ok(())
                } else {
                    Err(format!("{} lists {names:?}, not the shares", p.display()))
                }
            };
            for p in [root.join(".."), root.join("../.."), root.join("../x/../.."), collapse(&root).join("..")] {
                is_server(&p)?;
            }
            let back = root.join("..").join(&share_name).join(base.strip_prefix(&root).unwrap());
            if !same_file(&back, &base)? {
                return Err(format!("{} is not base", back.display()));
            }
            let c = e("canonicalize root/..", fs::canonicalize(root.join("..")))?;
            is_server(&c)?;
            Ok(format!("root/.. is the share list, canonicalize -> {}", c.display()))
        })());
        rep.check("relative-dotdot-to-server", (|| {
            // A relative ".." from the share root reaches the server
            // directory too, and from there the share names are the real
            // shares; ".." above the server stays there.
            let r = (|| {
                let same_server = |p: &Path| -> bool {
                    p.to_string_lossy().eq_ignore_ascii_case(&server.to_string_lossy())
                };
                e("chdir root", std::env::set_current_dir(&root))?;
                let names: Vec<String> = e("read_dir ..", fs::read_dir(".."))?
                    .flatten()
                    .map(|d| d.file_name().to_string_lossy().into_owned())
                    .collect();
                if !names.iter().any(|n| n.eq_ignore_ascii_case(&share_name)) {
                    return Err(format!(".. lists {names:?}"));
                }
                e("chdir ..", std::env::set_current_dir(".."))?;
                let at = e("getcwd", std::env::current_dir())?;
                if !same_server(&at) {
                    return Err(format!("cwd after .. is {}", at.display()));
                }
                e("chdir .. again", std::env::set_current_dir(".."))?;
                let at = e("getcwd", std::env::current_dir())?;
                if !same_server(&at) {
                    return Err(format!("cwd after ../.. is {}", at.display()));
                }
                let m = e("stat share by name", fs::metadata(&share_name))?;
                if !m.is_dir() || !same_file(Path::new(&share_name), &root)? {
                    return Err("share name does not resolve to the share".into());
                }
                e("chdir share", std::env::set_current_dir(&share_name))?;
                let at = e("getcwd", std::env::current_dir())?;
                if !same_file(&at, &root)? {
                    return Err(format!("cwd after entering share is {}", at.display()));
                }
                Ok(format!("root/.. is {}, back into {share_name}", server.display()))
            })();
            std::env::set_current_dir(&cwd).expect("chdir back");
            r
        })());
        rep.check("relative-canonicalize", (|| {
            let c = e("canonicalize rel", fs::canonicalize(Path::new(&format!("unc_paths-{pid}/c.txt"))))?;
            let s = c.to_string_lossy();
            if !s.starts_with("//") {
                return Err(format!("got {s}"));
            }
            Ok(s.into())
        })());
        rep.check("proc-self-cwd", (|| {
            let l = e("readlink /proc/self/cwd", fs::read_link("/proc/self/cwd"))?;
            if !same_file(&l, &cwd)? {
                return Err(format!("{} is not the cwd", l.display()));
            }
            Ok(l.display().to_string())
        })());
    } else {
        println!("SKIP UNC cases: cwd is not a //server/share path");
    }

    if let Ok(link) = std::env::var("RUST_APE_UNC_LINK") {
        let link = PathBuf::from(link);
        rep.check("dir-symlink-lstat", (|| {
            let m = e("symlink_metadata", fs::symlink_metadata(&link))?;
            if !m.is_symlink() {
                return Err(format!("reported as {:?}", m.file_type()));
            }
            Ok("is_symlink".into())
        })());
        rep.check("dir-symlink-readlink", (|| {
            let t = e("read_link", fs::read_link(&link))?;
            e("stat target", fs::metadata(&t))?;
            Ok(t.display().to_string())
        })());
        rep.check("dir-symlink-cwd-dotdot", (|| {
            // Entered through the link, ".." is the link's own parent.
            let r = (|| {
                e("chdir link", std::env::set_current_dir(&link))?;
                let parent = link.parent().ok_or("link has no parent")?;
                if !same_file(Path::new(".."), parent)? {
                    return Err(format!(".. is not {}", parent.display()));
                }
                e("chdir ..", std::env::set_current_dir(".."))?;
                let at = e("getcwd", std::env::current_dir())?;
                if !same_file(&at, parent)? {
                    return Err(format!("cwd after .. is {}", at.display()));
                }
                Ok(format!(".. from the link is {}", at.display()))
            })();
            std::env::set_current_dir(&cwd).expect("chdir back");
            r
        })());
        rep.check("dir-symlink-cwd-canonicalize", (|| {
            // With the link as cwd, "." canonicalizes to the target, not to
            // the spelling the cwd was entered with.
            let r = (|| {
                e("chdir link", std::env::set_current_dir(&link))?;
                let dot = e("canonicalize .", fs::canonicalize("."))?;
                let target = e("canonicalize link", fs::canonicalize(&link))?;
                if dot != target {
                    return Err(format!("{} is not {}", dot.display(), target.display()));
                }
                if same_path_text(&dot, &link) {
                    return Err(format!("{} still spells the link", dot.display()));
                }
                Ok(dot.display().to_string())
            })();
            std::env::set_current_dir(&cwd).expect("chdir back");
            r
        })());
        rep.check("dir-symlink-canonicalize", (|| {
            let c = e("canonicalize", fs::canonicalize(&link))?;
            if !same_file(&c, &link)? {
                return Err(format!("{} is not the link's target", c.display()));
            }
            Ok(c.display().to_string())
        })());
    }

    let _ = fs::remove_dir_all(&base);
    if rep.failed.is_empty() {
        println!("all path cases ok");
    } else {
        println!("FAILED: {}", rep.failed.join(", "));
        std::process::exit(1);
    }
}

