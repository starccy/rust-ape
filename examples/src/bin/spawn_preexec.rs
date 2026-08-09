//! The fork path of `Command::spawn`, the one `posix_spawn` doesn't take.
//!
//! A pre_exec closure, a uid, a chroot or a modified PATH with a bare program
//! name all make std skip posix_spawn and fork instead. On a linux target it
//! then builds the channel the child reports a failed exec on out of a
//! SOCK_SEQPACKET AF_UNIX pair, and XNU has no such thing, so the spawn used
//! to die with EPROTONOSUPPORT before it forked. shim/socket.c falls back to
//! a stream pair.
//!
//! Nothing else in examples/ reaches this path; every other one spawns by
//! absolute path with the environment untouched, which is exactly the shape
//! posix_spawn handles.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

const CHILD_ARG: &str = "--child";
const SID_ARG: &str = "--sid";
const MARKER: &str = "child reached exec";
/// Arbitrary, and nothing else in the path produces it, so seeing it come
/// back proves it travelled from the closure through the error channel.
const REFUSAL: i32 = 42;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == SID_ARG) {
        println!("{}", unsafe { libc::getsid(0) });
        return;
    }
    if args.iter().any(|a| a == CHILD_ARG) {
        println!("{MARKER}");
        return;
    }

    let exe = ape::program_executable_name().expect("program_executable_name");
    reaches_exec(&exe);
    carries_the_errno(&exe);
    setsid(&exe);

    println!("\nspawn pre_exec ok");
}

/// The plain case: a closure that does nothing still forces the fork path.
fn reaches_exec(exe: &str) {
    let out = unsafe {
        Command::new(exe)
            .arg(CHILD_ARG)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .pre_exec(|| Ok(()))
            .output()
    }
    .expect("spawn with a pre_exec closure");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(MARKER), "the child said {stdout:?}");
    assert!(out.status.success(), "the child exited {:?}", out.status.code());
    println!("a closure that succeeds: child ran and exited clean");
}

/// The part that actually needs the socket pair. A closure that refuses has
/// to reach the parent as its own errno, not as a child that started anyway.
fn carries_the_errno(exe: &str) {
    let err = unsafe {
        Command::new(exe)
            .arg(CHILD_ARG)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .pre_exec(|| Err(io::Error::from_raw_os_error(REFUSAL)))
            .spawn()
    }
    .expect_err("a pre_exec that refuses must fail the spawn");
    assert_eq!(err.raw_os_error(), Some(REFUSAL), "the child's errno came back as {err:?}");
    println!("a closure that refuses: errno {REFUSAL} came back over the error channel");
}

/// What portable-pty does between fork and exec, and the reason the gap was
/// found at all.
fn setsid(exe: &str) {
    let ours = unsafe { libc::getsid(0) };
    let out = unsafe {
        Command::new(exe)
            .arg(SID_ARG)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            })
            .output()
    }
    .expect("spawn with setsid in pre_exec");
    let text = String::from_utf8_lossy(&out.stdout);
    let theirs: i32 = text.trim().parse().unwrap_or_else(|_| panic!("child printed {text:?}"));
    // Getting this far already means setsid returned something other than -1,
    // since the closure turns that into a failed spawn. NT has no sessions to
    // ask about afterwards, and getsid answers -1 on both sides there.
    if ours < 0 {
        println!("setsid in the closure: ran, and this host has no session ids to compare");
        return;
    }
    assert_ne!(theirs, ours, "the child stayed in our session");
    println!("setsid in the closure: session {ours} -> {theirs}");
}
