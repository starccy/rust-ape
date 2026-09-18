use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::time::{Duration, Instant};

fn block(sig: i32) -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, sig);
        let rc = libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        assert_eq!(rc, 0, "sigprocmask: {}", std::io::Error::last_os_error());
        set
    }
}

fn sigwait(set: &libc::sigset_t) -> i32 {
    let mut got = 0;
    let rc = unsafe { libc::sigwait(set, &mut got) };
    assert_eq!(rc, 0, "sigwait returned {rc}");
    got
}

fn sigtimedwait(set: &libc::sigset_t, timeout: Duration) -> i32 {
    let ts = libc::timespec {
        tv_sec: timeout.as_secs() as _,
        tv_nsec: timeout.subsec_nanos() as _,
    };
    let got = unsafe { libc::sigtimedwait(set, std::ptr::null_mut(), &ts) };
    assert!(got > 0, "sigtimedwait: {}", std::io::Error::last_os_error());
    got
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--child") {
        let set = block(libc::SIGTERM);
        println!("child ready");
        let got = sigwait(&set);
        assert_eq!(got, libc::SIGTERM);
        return;
    }

    let set = block(libc::SIGALRM);

    let start = Instant::now();
    unsafe { libc::alarm(1) };
    assert_eq!(sigwait(&set), libc::SIGALRM);
    assert!(start.elapsed() >= Duration::from_millis(900), "alarm fired early");
    println!("sigwait consumed SIGALRM ok");

    unsafe { libc::alarm(1) };
    assert_eq!(sigtimedwait(&set, Duration::from_secs(5)), libc::SIGALRM);
    println!("sigtimedwait consumed SIGALRM ok");

    let exe = std::env::current_exe().expect("current_exe");
    let mut child = Command::new(&exe)
        .arg("--child")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn child");
    let mut stdout = child.stdout.take().unwrap();
    let mut line = [0u8; 16];
    let n = std::io::Read::read(&mut stdout, &mut line).expect("read child");
    assert!(n > 0, "child wrote nothing");
    std::thread::sleep(Duration::from_millis(200));
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let status = child.wait().expect("wait child");
    assert!(
        status.success(),
        "child did not consume SIGTERM: code {:?} signal {:?}",
        status.code(),
        status.signal()
    );
    println!("child consumed blocked SIGTERM ok");
}
