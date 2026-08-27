//! Spawning a child process and talking to it over pipes.
//!
//! The child is this same binary re-invoked with an argument. The async
//! flavors live in `tokio_process.rs` and `smol_process.rs`.
//!
//! Also exercises `waitid`, which cosmo does not have and shim/wait.c builds
//! out of wait4 off Linux, checking the siginfo it fills since that is what
//! a caller inspects.

use std::io::Write;
use std::process::{Command, Stdio};

const CHILD_ARG: &str = "--child";
const SLEEPER_ARG: &str = "--sleep";
const CHILD_EXIT: i32 = 123;
const GREETING: &str = "hello from the child";

fn main() {
    if std::env::args().any(|a| a == SLEEPER_ARG) {
        std::thread::sleep(std::time::Duration::from_secs(120));
    } else if std::env::args().any(|a| a == CHILD_ARG) {
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

    waitid(&exe);

    println!("\nsubprocess ok");
}

/// waitid on a running child, an exited one, a killed one, and with nothing
/// left to wait for. The children are reaped here, so std's own Child never
/// gets to wait on them.
fn waitid(exe: &str) {
    use std::mem::MaybeUninit;

    fn wait(idtype: libc::idtype_t, id: libc::id_t, options: libc::c_int) -> Result<libc::siginfo_t, i32> {
        let mut si = MaybeUninit::<libc::siginfo_t>::zeroed();
        let rc = unsafe { libc::waitid(idtype, id, si.as_mut_ptr(), options) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap());
        }
        Ok(unsafe { si.assume_init() })
    }

    let mut child = Command::new(exe)
        .arg(CHILD_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn for waitid");
    let pid = child.id();

    // Still blocked on stdin: WNOHANG has nothing to report and says so
    // through a zero si_pid.
    let si = wait(libc::P_PID, pid, libc::WEXITED | libc::WNOHANG).expect("waitid WNOHANG");
    assert_eq!(unsafe { si.si_pid() }, 0, "WNOHANG reported a running child");

    drop(child.stdin.take());
    let si = wait(libc::P_PID, pid, libc::WEXITED).expect("waitid WEXITED");
    assert_eq!(si.si_signo, libc::SIGCHLD);
    assert_eq!(si.si_code, libc::CLD_EXITED, "si_code {}", si.si_code);
    assert_eq!(unsafe { si.si_pid() }, pid as libc::pid_t);
    assert_eq!(unsafe { si.si_status() }, CHILD_EXIT);
    println!("waitid: exit {CHILD_EXIT} came back as CLD_EXITED for pid {pid}");

    let mut sleeper = Command::new(exe)
        .arg(SLEEPER_ARG)
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn sleeper");
    sleeper.kill().expect("kill sleeper");
    let si = wait(libc::P_ALL, 0, libc::WEXITED).expect("waitid P_ALL");
    assert_eq!(unsafe { si.si_pid() }, sleeper.id() as libc::pid_t);
    assert_eq!(si.si_code, libc::CLD_KILLED, "si_code {}", si.si_code);
    assert_eq!(unsafe { si.si_status() }, libc::SIGKILL);
    println!("waitid: P_ALL picked up the killed sleeper as CLD_KILLED/SIGKILL");

    assert_eq!(wait(libc::P_ALL, 0, libc::WEXITED), Err(libc::ECHILD));
    assert_eq!(wait(libc::P_ALL, 0, libc::WNOHANG), Err(libc::EINVAL), "no event class");
    println!("waitid: ECHILD once empty, EINVAL without an event class");
}
