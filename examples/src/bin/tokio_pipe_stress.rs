//! How many piped children a tokio runtime can hold at once.
//!
//! This exercises the chunked retry in shim/epoll.c. cosmo's poll() on
//! Windows refuses an array once it grows past a few dozen entries, and the
//! emulation flattens its whole interest list into one such call, so a
//! reactor holding every child's stdin and stdout is what drives that retry.
//! The runtime is `current_thread` and every step runs inside catch_unwind,
//! so a panic out of the io driver reads as which step failed rather than an
//! abort.
//!
//! Raise `RUST_APE_STRESS_CHILDREN` to push it further; 120 children with
//! 240 pipes takes about a second and a half on the Windows box.

use std::future::Future;
use std::io::{self, Write as _};
use std::panic::{self, AssertUnwindSafe};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::runtime::Runtime;

const CHILD_ARG: &str = "--echo-child";
/// Enough that the interest list outgrows what NT's poll() takes in one
/// call, so CI actually runs the chunked path.
const DEFAULT_CAP: usize = 80;

struct Kid {
    child: Child,
    stdin: ChildStdin,
    out: Lines<BufReader<ChildStdout>>,
}

fn main() {
    if std::env::args().any(|a| a == CHILD_ARG) {
        echo_child();
        return;
    }

    let cap = std::env::var("RUST_APE_STRESS_CHILDREN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CAP);

    let exe = ape::program_executable_name().expect("program_executable_name");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");

    println!("walking up to {cap} children, two pipes each");

    let mut kids: Vec<Kid> = Vec::new();
    let mut stopped_at = None;
    for n in 1..=cap {
        if let Err(e) = step(&rt, &mut kids, &exe, n) {
            stopped_at = Some((n, e));
            break;
        }
        if n % 5 == 0 || n == cap {
            println!("  {n} children alive, {} pipes registered", n * 2);
        }
    }

    if let Some((n, e)) = stopped_at {
        // Nothing else will run on this runtime, its driver is what broke.
        std::mem::forget(kids);
        panic!(
            "stopped going from {} to {n} children, with {} pipes already registered: {e}",
            n - 1,
            (n - 1) * 2
        );
    }
    println!("\n{cap} children, {} pipes, all answering", cap * 2);
    report(&rt, &mut kids);

    println!("\ntokio pipe stress ok");
}

/// Spawn one more child, then make every living child answer, which is what
/// forces the whole interest list through a single poll.
fn step(rt: &Runtime, kids: &mut Vec<Kid>, exe: &str, round: usize) -> Result<(), String> {
    let kid = guard(rt, async {
        let mut child = Command::new(exe)
            .arg(CHILD_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().expect("child stdin");
        let out = BufReader::new(child.stdout.take().expect("child stdout")).lines();
        Ok(Kid { child, stdin, out })
    })?;
    kids.push(kid);
    guard(rt, poke(kids, round))
}

async fn poke(kids: &mut [Kid], round: usize) -> io::Result<()> {
    for (i, k) in kids.iter_mut().enumerate() {
        k.stdin.write_all(format!("{round}-{i}\n").as_bytes()).await?;
        k.stdin.flush().await?;
    }
    for (i, k) in kids.iter_mut().enumerate() {
        let line = tokio::time::timeout(Duration::from_secs(20), k.out.next_line())
            .await
            .map_err(|_| io::Error::other(format!("child {i} never answered round {round}")))?
            ?
            .ok_or_else(|| io::Error::other(format!("child {i} closed stdout")))?;
        let want = format!("echo {round}-{i}");
        if line != want {
            return Err(io::Error::other(format!("child {i} said {line:?}, wanted {want:?}")));
        }
    }
    Ok(())
}

/// Run one future to completion, turning both an error and a driver panic
/// into the same string.
fn guard<F, T>(rt: &Runtime, f: F) -> Result<T, String>
where
    F: Future<Output = io::Result<T>>,
{
    match panic::catch_unwind(AssertUnwindSafe(|| rt.block_on(f))) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!("{e} (kind {:?})", e.kind())),
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "panic with no message".into());
            Err(format!("panicked: {msg}"))
        }
    }
}

/// Wind the children down in order, so a clean run also proves the reactor
/// survives a hundred deregistrations.
fn report(rt: &Runtime, kids: &mut Vec<Kid>) {
    let n = kids.len();
    let r = panic::catch_unwind(AssertUnwindSafe(|| {
        rt.block_on(async {
            for mut k in kids.drain(..) {
                drop(k.stdin);
                while k.out.next_line().await.ok().flatten().is_some() {}
                let _ = k.child.wait().await;
            }
        })
    }));
    assert!(r.is_ok(), "the reactor died while shutting {n} children down");
    println!("all {n} children closed and reaped");
}

fn echo_child() {
    let stdin = io::stdin();
    let mut out = io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        match io::BufRead::read_line(&mut stdin.lock(), &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let _ = writeln!(out, "echo {}", line.trim_end());
        let _ = out.flush();
    }
}
