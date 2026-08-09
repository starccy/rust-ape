//! Watching the filesystem through `notify`, which means inotify.
//!
//! cosmo has no inotify, so shim/inotify.c answers the real syscalls on
//! Linux or a scan-and-diff emulation everywhere else. Set
//! RUST_APE_INOTIFY_EMULATE=1 to run the emulation on a machine that
//! has the real thing.
//!
//! The emulation reports less than the kernel does, which is what shapes the
//! assertions below. It sees creations, deletions and content changes; it
//! cannot see opens and closes, and a rename reaches it as a delete plus a
//! create. So every phase here waits for a path to be mentioned at all rather
//! than for a particular event kind, and gives it long enough for a scan to
//! come round.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, RecursiveMode, Watcher};

// Comfortably above the emulation's 200ms scan interval, and never reached
// when the kernel is answering.
const PATIENCE: Duration = Duration::from_secs(10);

fn main() {
    let dir = std::env::temp_dir().join(format!("rust-ape-watch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    
    let dir = std::fs::canonicalize(&dir).expect("canonicalize scratch dir");
    println!("watching {}", dir.display());

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(tx).expect("recommended_watcher");
    watcher
        .watch(&dir, RecursiveMode::Recursive)
        .expect("watch the scratch dir");

    let file = dir.join("one.txt");
    std::fs::write(&file, b"first").expect("create a file");
    expect(&rx, &file, "creating a file");

    std::fs::write(&file, b"first, and then quite a lot more").expect("grow the file");
    expect(&rx, &file, "growing it");

    // Recursive watching is not one watch, it is one per directory, added as
    // directories turn up. A file inside a directory created after the watch
    // started is what proves the new watch took.
    let sub = dir.join("sub");
    std::fs::create_dir(&sub).expect("create a subdirectory");
    expect(&rx, &sub, "creating a subdirectory");

    // Under a real inotify there is a race here that the emulation does not
    // have: the watch on `sub` is only added once notify has processed the
    // event announcing it, and a file written before that lands unseen. The
    // kernel has always behaved this way. Writing again until something
    // arrives is what a real watcher does about it.
    let nested = sub.join("two.txt");
    let mut round = 0;
    expect_while(&rx, &nested, "creating a file inside it", || {
        round += 1;
        std::fs::write(&nested, format!("nested {round}")).expect("create a nested file");
    });

    std::fs::remove_file(&nested).expect("remove the nested file");
    expect(&rx, &nested, "removing it");

    watcher.unwatch(&dir).expect("unwatch");
    drop(watcher);
    std::fs::remove_dir_all(&dir).ok();

    println!("\nfile watch ok");
}

/// Drains events until one names `path`, or gives up.
fn expect(rx: &mpsc::Receiver<notify::Result<Event>>, path: &Path, what: &str) {
    expect_while(rx, path, what, || {})
}

/// The same, running `poke` once at the start and again every second until an
/// event turns up.
fn expect_while(
    rx: &mpsc::Receiver<notify::Result<Event>>,
    path: &Path,
    what: &str,
    mut poke: impl FnMut(),
) {
    let deadline = Instant::now() + PATIENCE;
    let mut next_poke = Instant::now();
    let mut seen: Vec<PathBuf> = Vec::new();
    loop {
        let now = Instant::now();
        assert!(
            now < deadline,
            "no event for {} after {what}; saw {seen:?}",
            path.display()
        );
        if now >= next_poke {
            poke();
            next_poke = now + Duration::from_secs(1);
        }
        let left = std::cmp::min(deadline, next_poke).saturating_duration_since(now);
        match rx.recv_timeout(left) {
            Ok(Ok(ev)) => {
                if ev.paths.iter().any(|p| p == path) {
                    println!("  {what}: {:?}", ev.kind);
                    return;
                }
                seen.extend(ev.paths);
            }
            // A watcher error is worth reporting but not worth failing on: the
            // emulation reports queue overflow the same way the kernel does.
            Ok(Err(e)) => println!("  watcher error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {} // poke again, or give up
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("watcher hung up during {what}"),
        }
    }
}
