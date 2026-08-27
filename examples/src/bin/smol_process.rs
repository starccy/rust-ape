//! Async subprocesses on smol. The child is this same binary re-invoked with
//! a flag, the same shape as `tokio_process.rs`.
//!
//! async-process picks its reaper at runtime: pidfd first, which cosmo does
//! not have, then SIGCHLD through async-signal. Both hosts land on the
//! signal backend here, so this is what exercises SIGCHLD delivery to a
//! process that is never inside wait4 when the child exits.

use std::time::Duration;

use smol::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use smol::process::{Command, Stdio};
use smol::stream::StreamExt;

const CHILD_ARG: &str = "--child";
const SLEEPER_ARG: &str = "--sleep";
const CHILD_EXIT: i32 = 123;
const GREETING: &str = "hello from the child";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == SLEEPER_ARG) {
        std::thread::sleep(Duration::from_secs(120));
        return;
    }
    if args.iter().any(|a| a == CHILD_ARG) {
        child();
        return;
    }

    smol::block_on(async {
        let exe = ape::program_executable_name().expect("program_executable_name");
        println!("re-invoking {exe}");
        output(&exe).await;
        pipes(&exe).await;
        concurrent(&exe).await;
        killing(&exe).await;
        missing().await;
    });

    println!("\nsmol process ok");
}

fn child() {
    use std::io::{BufRead, Write};
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).expect("child read stdin");
    println!("{GREETING}");
    println!("echo: {}", line.trim_end());
    std::io::stdout().flush().expect("child flush");
    eprintln!("child stderr");
    std::process::exit(CHILD_EXIT);
}

async fn output(exe: &str) {
    let out = Command::new(exe)
        .arg(CHILD_ARG)
        .stdin(Stdio::null())
        .output()
        .await
        .expect("output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(GREETING), "child stdout was {stdout:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("child stderr"),
        "child stderr came back empty"
    );
    assert_eq!(out.status.code(), Some(CHILD_EXIT), "child exited {:?}", out.status.code());
    println!("output(): collected both streams and exit {CHILD_EXIT}");
}

async fn pipes(exe: &str) {
    let mut child = Command::new(exe)
        .arg(CHILD_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"ping\n")
        .await
        .expect("write to child");

    let mut lines = BufReader::new(child.stdout.take().expect("child stdout")).lines();
    let mut seen = Vec::new();
    while let Some(line) = lines.next().await {
        seen.push(line.expect("next line"));
    }
    assert!(seen.iter().any(|l| l == GREETING), "child said {seen:?}");
    assert!(seen.iter().any(|l| l == "echo: ping"), "child never saw our stdin: {seen:?}");

    let status = child.status().await.expect("status");
    assert_eq!(status.code(), Some(CHILD_EXIT));
    println!("pipes: wrote stdin, read stdout line by line, then waited");
}

/// Eight children at once is where a reaper that only handles one SIGCHLD at a
/// time falls over.
async fn concurrent(exe: &str) {
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let exe = exe.to_string();
            smol::spawn(async move {
                Command::new(&exe)
                    .arg(CHILD_ARG)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .expect("status")
                    .code()
            })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await, Some(CHILD_EXIT), "one of the children was misreaped");
    }
    println!("concurrent: 8 children spawned and reaped together");
}

async fn killing(exe: &str) {
    let mut child = Command::new(exe)
        .arg(SLEEPER_ARG)
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn sleeper");
    smol::Timer::after(Duration::from_millis(100)).await;
    assert!(child.try_status().expect("try_status").is_none(), "sleeper exited early");
    child.kill().expect("kill");
    let status = child.status().await.expect("status after kill");
    assert!(!status.success(), "killed child reported success");
    println!("killing: sleeper killed and reaped, status {status}");
}

async fn missing() {
    let err = Command::new("definitely-not-a-real-program-xyzzy")
        .spawn()
        .expect_err("spawning a nonexistent program succeeded");
    println!("missing: {:?}", err.kind());
}
