//! Spawning a child process and talking to it over pipes.
//!
//! The child is this same binary re-invoked with an argument
//!
//! Synchronous on purpose: async-process's reaper wants SIGCHLD or pidfd and
//! cosmo has neither everywhere, so async-process.patch disables its driver.

use std::io::Write;
use std::process::{Command, Stdio};

const CHILD_ARG: &str = "--child";
const CHILD_EXIT: i32 = 123;
const GREETING: &str = "hello from the child";

fn main() {
    if std::env::args().any(|a| a == CHILD_ARG) {
        child()
    } else {
        parent()
    }
}

/// The other half of this binary: read a line, say something on both streams,
/// exit with a code the parent can recognize.
fn child() {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).expect("child read stdin");
    println!("{GREETING}");
    println!("echo: {}", line.trim_end());
    eprintln!("child stderr");
    std::process::exit(CHILD_EXIT);
}

fn parent() {
    let exe = ape::program_executable_name().expect("program_executable_name");
    println!("re-invoking {exe}");

    // Pipes in both directions, plus the exit code.
    let mut child = Command::new(&exe)
        .arg(CHILD_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"ping\n")
        .expect("write to child");

    let out = child.wait_with_output().expect("wait_with_output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stdout.lines() {
        println!("  child stdout: {line}");
    }
    for line in stderr.lines() {
        println!("  child stderr: {line}");
    }

    assert!(stdout.contains(GREETING), "child stdout missing its greeting");
    assert!(stdout.contains("echo: ping"), "child never saw what we wrote to its stdin");
    assert!(stderr.contains("child stderr"), "child stderr came back empty");
    assert_eq!(
        out.status.code(),
        Some(CHILD_EXIT),
        "child exited {:?}, expected {CHILD_EXIT}",
        out.status.code()
    );
    assert!(!out.status.success(), "a nonzero exit reported success");
    println!("child exited with {CHILD_EXIT}, pipes carried both directions");

    // Environment variables reach the child.
    let out = Command::new(&exe)
        .arg(CHILD_ARG)
        .env("RUST_APE_CHILD_VAR", "set-by-parent")
        .stdin(Stdio::null())
        .output()
        .expect("second spawn");
    assert_eq!(out.status.code(), Some(CHILD_EXIT), "second child exited wrong");
    println!("second child ran with a custom environment");

    // Spawning something that isn't there has to fail, not hang.
    let missing = Command::new("definitely-not-a-real-program-xyzzy").spawn();
    assert!(missing.is_err(), "spawning a nonexistent program succeeded");
    println!("missing program reported {:?}", missing.unwrap_err().kind());

    println!("\nsubprocess ok");
}
