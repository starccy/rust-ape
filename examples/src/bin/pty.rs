//! A pseudo-terminal, opened and driven from an APE binary.
//!
//! cosmo has openpty and forkpty on unix, so opening the pty is the easy
//! part. The child then needs setsid and ioctl(TIOCSCTTY) between fork and
//! exec to claim the slave as its controlling terminal, which shim/termios.c
//! answers; before it did, every pty spawn stopped at ENOTTY.
//!
//! No terminal is required to run this. openpty makes a new one.
//!
//! Windows has no pty at all, so this example reports rather than fails
//! there. cosmo imports CreatePseudoConsole but never wires it to openpty,
//! which answers ENOSYS.

use std::io::Read;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize};

const MARKER: &str = "PTY_HELLO";

fn main() {
    println!("host: {:?}", ape::current_os());

    let sys = portable_pty::native_pty_system();
    let size = PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = match sys.openpty(size) {
        Ok(p) => p,
        Err(e) if ape::is_windows() => {
            println!("openpty: {e}");
            println!("\npty skipped, cosmo has no pty on NT");
            return;
        }
        Err(e) => panic!("openpty: {e}"),
    };
    println!("openpty: 24x80");

    let (prog, args): (&str, &[&str]) = if ape::is_windows() {
        // cosmo resolves a program name the unix way, so the suffix is not
        // optional here.
        ("cmd.exe", &["/c", "echo PTY_HELLO"])
    } else {
        ("/bin/sh", &["-c", "echo PTY_HELLO"])
    };
    let mut cmd = CommandBuilder::new(prog);
    for a in args {
        cmd.arg(a);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("spawn a child on the slave side");
    // The master only sees EOF once every handle to the slave is gone.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("clone the master reader");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || out.len() > 4096 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        tx.send(out).ok();
    });

    let status = child.wait().expect("wait for the child");
    assert!(status.success(), "the child exited {status:?}");

    let out = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the master never reached EOF");
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains(MARKER),
        "the pty did not carry the child's output; got {text:?}"
    );
    // A pty turns \n into \r\n on the way out, which is the line discipline
    // doing its job and a decent sign this is a terminal and not a pipe.
    println!(
        "read {} bytes back through the master, CRLF translation {}",
        out.len(),
        if text.contains("\r\n") { "on" } else { "off" }
    );

    println!("\npty ok");
}
