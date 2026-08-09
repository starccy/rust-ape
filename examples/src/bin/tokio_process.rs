//! Async subprocesses. The child is this same binary re-invoked with a flag.
//!
//! tokio reaps children through a SIGCHLD handler on Unix targets, so spawn,
//! wait and kill all lean on cosmo's signal emulation here. Sync
//! `std::process::Command` is covered separately by `subprocess.rs`.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        let exe = ape::program_executable_name().expect("program_executable_name");
        println!("re-invoking {exe}");
        output(&exe).await;
        pipes(&exe).await;
        concurrent(&exe).await;
        killing(&exe).await;
        missing().await;
    });

    println!("\ntokio process ok");
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
        .stdin(std::process::Stdio::null())
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
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
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
    while let Some(line) = lines.next_line().await.expect("next_line") {
        seen.push(line);
    }
    assert!(seen.iter().any(|l| l == GREETING), "child said {seen:?}");
    assert!(seen.iter().any(|l| l == "echo: ping"), "child never saw our stdin: {seen:?}");

    let status = child.wait().await.expect("wait");
    assert_eq!(status.code(), Some(CHILD_EXIT));
    println!("pipes: wrote stdin, read stdout line by line, then waited");
}

/// Eight children at once is where a reaper that only handles one SIGCHLD at a
/// time falls over.
async fn concurrent(exe: &str) {
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let exe = exe.to_string();
            tokio::spawn(async move {
                Command::new(&exe)
                    .arg(CHILD_ARG)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await
                    .expect("status")
                    .code()
            })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await.expect("task"), Some(CHILD_EXIT), "one of the children was misreaped");
    }
    println!("concurrent: 8 children spawned and reaped together");
}

async fn killing(exe: &str) {
    let mut child = Command::new(exe)
        .arg(SLEEPER_ARG)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleeper");

    // It sleeps for two minutes, so waiting on it must not return on its own.
    let r = tokio::time::timeout(Duration::from_millis(200), child.wait()).await;
    assert!(r.is_err(), "the sleeper exited early");

    child.kill().await.expect("kill");
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("wait after kill hung")
        .expect("wait");
    assert!(!status.success(), "a killed child reported success");
    println!("kill(): sleeper survived a 200ms wait, then died on demand");
}

async fn missing() {
    let r = Command::new("definitely-not-a-real-program-xyzzy").output().await;
    assert!(r.is_err(), "spawning a nonexistent program succeeded");
    println!("missing program reported {:?}", r.unwrap_err().kind());
}
