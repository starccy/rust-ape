//! The cross-process side of /proc: what one process sees of another. A
//! child of this process is the observed subject, so every fact has a known
//! ground truth (its cwd, an env var, its argv, a file and a socket it
//! holds, threads it started). Linux has the real thing; Windows and Apple
//! Silicon macOS run the emulation in shim/procfs/. NT cannot enumerate
//! another process's descriptors, so the fd case only expects its socket
//! there; everything else must hold on all three. The BSDs skip.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use rust_ape_examples::Report;

const TOKEN: &str = "PROC_OTHERS_TOKEN";

// The observed subject: pins its cwd, holds an open file and a listening
// socket, runs two extra threads, then waits for stdin to close.
fn child_main() {
    let dir = std::env::temp_dir();
    std::env::set_current_dir(&dir).unwrap();
    let file = dir.join(format!("proc-others-{}.txt", std::process::id()));
    fs::write(&file, "held open").unwrap();
    let held = fs::File::open(&file).unwrap();
    let sock = TcpListener::bind("127.0.0.1:0").unwrap();
    let _threads: Vec<_> = (0..2)
        .map(|_| std::thread::spawn(|| std::thread::sleep(Duration::from_secs(600))))
        .collect();
    // the canonical spelling of the cwd is the ground truth for the cwd
    // link; /tmp is an alias on some hosts, so the parent cannot know it
    let cwd = fs::canonicalize(".").unwrap();
    println!("ready {}|{}", file.display(), cwd.display());
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line); // parent closes to end us
    drop(held);
    drop(sock);
    let _ = fs::remove_file(&file);
}

fn read_dir_names(path: &str) -> Result<Vec<String>, String> {
    let rd = fs::read_dir(path).map_err(|e| format!("read_dir {path}: {e}"))?;
    let mut v = Vec::new();
    for ent in rd {
        v.push(ent.map_err(|e| e.to_string())?.file_name().to_string_lossy().into_owned());
    }
    Ok(v)
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--child") {
        child_main();
        return;
    }

    let mut rep = Report::new();

    let exe = std::env::current_exe().expect("current_exe");
    let mut child = Command::new(&exe)
        .arg("--child")
        .env(TOKEN, "witness-me")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn child");
    let pid = child.id();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = lines.next().expect("child died early").expect("child stdout");
    let ready = ready.strip_prefix("ready ").expect("ready line");
    let (held_file, child_cwd) = ready.split_once('|').expect("ready fields");
    let (held_file, child_cwd) = (held_file.to_string(), child_cwd.to_string());
    // give the child a moment to settle its threads
    std::thread::sleep(Duration::from_millis(300));

    let base = format!("/proc/{pid}");
    let exe_stem = exe
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();

    rep.check("child-comm", (|| {
        let comm = fs::read_to_string(format!("{base}/comm")).map_err(|e| e.to_string())?;
        let comm = comm.trim().to_lowercase();
        let n = comm.len().min(exe_stem.len()).min(15);
        if n == 0 || comm[..n] != exe_stem[..n] {
            return Err(format!("comm {comm:?} vs binary {exe_stem:?}"));
        }
        Ok(comm)
    })());

    rep.check("child-cmdline", (|| {
        let raw = fs::read(format!("{base}/cmdline")).map_err(|e| e.to_string())?;
        let args: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        if args.len() < 2 || args.last().map(String::as_str) != Some("--child") {
            return Err(format!("argv {args:?}"));
        }
        let argv0 = args[0].to_lowercase();
        let loaderish = argv0.ends_with("/ape") || argv0 == "ape" || argv0.contains(".ape-");
        // On Linux the kernel shows the loader binfmt ran; that is the real
        // /proc and stays untouched. The emulated hosts answer the program.
        if ape::is_linux() {
            if !argv0.contains(&exe_stem) && !loaderish {
                return Err(format!("argv[0] {argv0:?} is neither {exe_stem:?} nor a loader"));
            }
        } else {
            if !argv0.contains(&exe_stem) {
                return Err(format!("argv[0] {argv0:?} does not name {exe_stem:?}"));
            }
            if loaderish {
                return Err(format!("argv[0] {argv0:?} is the loader"));
            }
        }
        Ok(format!("{} args, argv[0] {}", args.len(), args[0]))
    })());

    rep.check("child-exe", (|| {
        let target = fs::read_link(format!("{base}/exe")).map_err(|e| e.to_string())?;
        let name = target
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let stem = target
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let loaderish = name == "ape" || name.starts_with(".ape-");
        // Linux truthfully names the loader binfmt ran; see child-cmdline
        if stem != exe_stem && !(ape::is_linux() && loaderish) {
            return Err(format!("exe {} vs {}", target.display(), exe.display()));
        }
        if !target.is_absolute() {
            return Err(format!("exe {} not absolute", target.display()));
        }
        fs::metadata(&target).map_err(|e| format!("exe target unstatable: {e}"))?;
        Ok(target.display().to_string())
    })());

    rep.check("child-cwd", (|| {
        let cwd = fs::read_link(format!("{base}/cwd")).map_err(|e| e.to_string())?;
        let got = fs::canonicalize(&cwd).map_err(|e| format!("canonicalize {}: {e}", cwd.display()))?;
        // compare case-blind; one side may spell an NT path differently
        if got.to_string_lossy().to_lowercase() != child_cwd.to_lowercase() {
            return Err(format!("cwd {} vs child's own {}", got.display(), child_cwd));
        }
        Ok(got.display().to_string())
    })());

    rep.check("child-environ", (|| {
        let raw = fs::read(format!("{base}/environ")).map_err(|e| e.to_string())?;
        let s = String::from_utf8_lossy(&raw);
        if !s.split('\0').any(|kv| kv == "PROC_OTHERS_TOKEN=witness-me") {
            return Err(format!("{TOKEN} missing among {} vars", s.split('\0').filter(|x| !x.is_empty()).count()));
        }
        Ok(format!("{} vars, token seen", s.split('\0').filter(|x| !x.is_empty()).count()))
    })());

    rep.check("child-task", (|| {
        let names = read_dir_names(&format!("{base}/task"))?;
        let tids: Vec<&String> = names.iter().filter(|n| n.chars().all(|c| c.is_ascii_digit())).collect();
        // main plus the two sleepers; hosts that cannot list threads answer
        // one entry, which is a regression here on all three platforms
        if tids.len() < 3 {
            return Err(format!("{} thread entries: {names:?}", tids.len()));
        }
        let stat = fs::read_to_string(format!("{base}/task/{}/stat", tids[0])).map_err(|e| e.to_string())?;
        if stat.split_whitespace().count() < 20 {
            return Err(format!("task stat too short: {stat:?}"));
        }
        Ok(format!("{} threads, task/{}/stat readable", tids.len(), tids[0]))
    })());

    rep.check("child-fd", (|| {
        let names = read_dir_names(&format!("{base}/fd"))?;
        let fds: Vec<&String> = names.iter().filter(|n| n.chars().all(|c| c.is_ascii_digit())).collect();
        if fds.is_empty() {
            return Err("no fd entries".into());
        }
        let mut targets = Vec::new();
        for fd in &fds {
            if let Ok(t) = fs::read_link(format!("{base}/fd/{fd}")) {
                targets.push(t.to_string_lossy().into_owned());
            }
        }
        let has_sock = targets.iter().any(|t| t.starts_with("socket:["));
        if ape::is_windows() {
            // NT sees only the socket tables of other processes
            if !has_sock {
                return Err(format!("no socket among {targets:?}"));
            }
            return Ok(format!("{} sockets (NT sees only sockets)", targets.len()));
        }
        // the full table: stdio, the held file, the listener
        let held = Path::new(&held_file)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let has_file = targets.iter().any(|t| t.ends_with(&held));
        if !has_file || !has_sock {
            return Err(format!("held file {held:?} seen: {has_file}, socket seen: {has_sock}, targets {targets:?}"));
        }
        Ok(format!("{} fds, file and socket links resolve", fds.len()))
    })());

    rep.check("other-processes", (|| {
        // at least one process that is not us or the child answers comm
        let mut seen = 0;
        for name in read_dir_names("/proc")? {
            if !name.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if name == pid.to_string() || name == std::process::id().to_string() {
                continue;
            }
            if fs::read_to_string(format!("/proc/{name}/comm")).is_ok() {
                seen += 1;
                if seen >= 3 {
                    break;
                }
            }
        }
        if seen == 0 {
            return Err("no other process's comm readable".into());
        }
        Ok(format!("{seen}+ other processes answer comm"))
    })());

    drop(child.stdin.take()); // release the child
    let _ = child.wait();

    rep.finish("proc others ok");
}
