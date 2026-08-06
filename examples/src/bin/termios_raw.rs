//! termios / tty-ioctl round-trip through the shim (shim/termios.c).
//!
//! Two tiers. The pipe/file assertions (FIONREAD translation, ENOTTY for
//! tty ioctls on non-ttys) always run; the raw-mode assertions need a real
//! terminal and are skipped when stdin isn't one. To run the full set
//! non-interactively on Linux, use a pty:
//!
//!     script -qec ./termios_raw.com /dev/null
//!
//! The tty tier checks that tcgetattr repacks cosmo's struct into musl's
//! layout, cfmakeraw operates in musl coding, tcsetattr repacks back, and
//! the mode actually changed on the host.

use std::os::fd::AsRawFd;

fn tty_assertions(fd: i32) {
    let mut orig: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::tcgetattr(fd, &mut orig) },
        0,
        "tcgetattr: {:?}",
        std::io::Error::last_os_error()
    );
    // A terminal at rest is in cooked mode: canonical, echoing.
    assert!(orig.c_lflag & libc::ICANON != 0, "expected ICANON in cooked mode");
    assert!(orig.c_lflag & libc::ECHO != 0, "expected ECHO in cooked mode");

    // Flip to raw and read the mode back through the full repack chain.
    let mut raw = orig;
    unsafe { libc::cfmakeraw(&mut raw) };
    assert_eq!(raw.c_lflag & libc::ICANON, 0);
    assert_eq!(raw.c_cc[libc::VMIN], 1);
    assert_eq!(raw.c_cc[libc::VTIME], 0);
    assert_eq!(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) }, 0, "tcsetattr raw");

    let mut back: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(fd, &mut back) }, 0);
    assert_eq!(back.c_lflag & libc::ICANON, 0, "raw mode did not stick (ICANON)");
    assert_eq!(back.c_lflag & libc::ECHO, 0, "raw mode did not stick (ECHO)");
    // The NT console always processes output (that's how its VT sequences
    // work) and has no character-size notion, so cosmo reports OPOST set
    // and no CS bits there. Host semantics, not a translation gap — TUIs
    // don't depend on either.
    let nt_console = std::fs::metadata("C:\\Windows").is_ok();
    if !nt_console {
        assert_eq!(back.c_oflag & libc::OPOST, 0, "raw mode did not stick (OPOST)");
        assert_eq!(back.c_cflag & libc::CSIZE, libc::CS8, "CS8 did not survive");
    }
    assert_eq!(back.c_cc[libc::VMIN], 1, "VMIN did not survive the repack");
    assert_eq!(back.c_cc[libc::VTIME], 0, "VTIME did not survive the repack");

    // Restore, and verify restoration took.
    assert_eq!(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &orig) }, 0, "restore");
    let mut fin: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(fd, &mut fin) }, 0);
    assert!(fin.c_lflag & libc::ICANON != 0, "restore did not stick");

    // Window size: the winsize struct is shape-identical, only the request
    // code is translated. Dimensions may legitimately be 0 under a detached
    // pty, so only the call's success is asserted.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) },
        0,
        "TIOCGWINSZ: {:?}",
        std::io::Error::last_os_error()
    );
    println!("tty tier ok (winsize {}x{})", ws.ws_col, ws.ws_row);
}

fn main() {
    // Pipe tier: FIONREAD through the translated request code.
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let payload = b"hello";
    assert_eq!(
        unsafe { libc::write(fds[1], payload.as_ptr().cast(), payload.len()) },
        payload.len() as isize
    );
    let mut n: libc::c_int = 0;
    assert_eq!(
        unsafe { libc::ioctl(fds[0], libc::FIONREAD, &mut n) },
        0,
        "FIONREAD: {:?}",
        std::io::Error::last_os_error()
    );
    assert_eq!(n, payload.len() as i32, "FIONREAD count");

    // tty-config ioctl on a non-tty must come back ENOTTY, in musl coding.
    let f = std::fs::File::open(std::env::args().next().unwrap()).expect("open self");
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::ioctl(f.as_raw_fd(), libc::TCGETS, &mut t) }, -1);
    let e = std::io::Error::last_os_error().raw_os_error().unwrap();
    assert_eq!(e, libc::ENOTTY, "want ENOTTY(25), got {e}");
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }

    if unsafe { libc::isatty(0) } == 1 {
        tty_assertions(0);
        println!("termios_raw: all assertions passed");
    } else {
        println!("termios_raw: pipe tier passed; no tty on stdin, tty tier skipped");
        println!("  (run under `script -qec ./termios_raw.com /dev/null` for the full set)");
    }
}
