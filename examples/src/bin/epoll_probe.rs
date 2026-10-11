//! epoll, exercised directly rather than through mio.
//!
//! The fork provides it three ways: the raw syscalls on Linux, a kqueue on
//! XNU and the BSDs, and an interest list over poll() on Windows. Everything
//! here is what mio's selector actually does to them, in the order it does
//! it, so a break shows up as a failed assert rather than as an async
//! runtime that quietly stops waking up.
//!
//! Reporting an event twice is allowed where the host's model differs (a
//! kqueue keeps one filter per direction). Reporting it zero times is not,
//! and neither is reporting an idle socket's writability over and over,
//! which is the case that once kept tokio at 100% CPU.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;

/// The wait side, with a generous timeout, returning what came back.
fn wait(ep: i32, max: usize, timeout_ms: i32) -> Vec<libc::epoll_event> {
    let mut evs: Vec<libc::epoll_event> = (0..max)
        .map(|_| libc::epoll_event { events: 0, u64: 0 })
        .collect();
    let n = unsafe { libc::epoll_wait(ep, evs.as_mut_ptr(), max as i32, timeout_ms) };
    assert!(
        n >= 0,
        "epoll_wait failed: {}",
        std::io::Error::last_os_error()
    );
    evs.truncate(n as usize);
    evs
}

fn ctl(ep: i32, op: i32, fd: i32, events: u32, token: u64) {
    let mut ev = libc::epoll_event { events, u64: token };
    let rc = unsafe { libc::epoll_ctl(ep, op, fd, &mut ev) };
    assert_eq!(
        rc,
        0,
        "epoll_ctl(op={op}, fd={fd}) failed: {}",
        std::io::Error::last_os_error()
    );
}

/// `epoll_event` is packed on x86_64, so taking a reference to a field is
/// undefined behavior and every assert below would do exactly that. Copy the
/// two fields out by value first.
fn only(evs: &[libc::epoll_event], what: &str) -> (u32, u64) {
    assert_eq!(
        evs.len(),
        1,
        "{what}: expected exactly one ready fd, got {}",
        evs.len()
    );
    (evs[0].events, evs[0].u64)
}

fn main() {
    // The struct crosses the shim boundary unrepacked, so the two sides have
    // to already agree. It is packed on x86_64 and naturally aligned on
    // aarch64, which is exactly why this is worth pinning.
    #[cfg(target_arch = "x86_64")]
    assert_eq!(
        std::mem::size_of::<libc::epoll_event>(),
        12,
        "epoll_event is not packed"
    );
    #[cfg(target_arch = "aarch64")]
    assert_eq!(
        std::mem::size_of::<libc::epoll_event>(),
        16,
        "epoll_event grew padding"
    );

    let ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    assert!(
        ep >= 0,
        "epoll_create1 failed: {}",
        std::io::Error::last_os_error()
    );
    println!("epoll_create1 -> fd {ep}");

    // An empty set must time out rather than report anything.
    let evs = wait(ep, 8, 50);
    assert!(
        evs.is_empty(),
        "an empty epoll reported {} events",
        evs.len()
    );
    println!("empty set times out cleanly");

    // A connected pair, so readiness is something we control.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let mut client = TcpStream::connect(addr).expect("connect");
    let (mut server, _) = listener.accept().expect("accept");
    server.set_nonblocking(true).expect("set_nonblocking");
    let sfd = server.as_raw_fd();

    const READ: u32 = libc::EPOLLIN as u32 | libc::EPOLLRDHUP as u32 | libc::EPOLLET as u32;
    const TOKEN: u64 = 0x5eed;

    ctl(ep, libc::EPOLL_CTL_ADD, sfd, READ, TOKEN);
    println!("registered fd {sfd} for EPOLLIN|EPOLLRDHUP|EPOLLET");

    // Nothing written yet, so still nothing to report.
    assert!(
        wait(ep, 8, 50).is_empty(),
        "reported readable before anything was sent"
    );

    client.write_all(b"first").expect("client write");
    let (events, token) = only(&wait(ep, 8, 2000), "first write");
    assert_eq!(token, TOKEN, "the token came back changed");
    assert_ne!(
        events & libc::EPOLLIN as u32,
        0,
        "ready but not for reading"
    );
    println!("first write reported: events={events:#x} token={token:#x}");

    let mut buf = [0u8; 32];
    let n = server.read(&mut buf).expect("server read");
    assert_eq!(&buf[..n], b"first");

    // The part every poll-backed selector gets wrong. A registration is not
    // consumed by being reported; the second write has to arrive too, with no
    // re-registration in between.
    client.write_all(b"second").expect("client write again");
    let (_, token) = only(
        &wait(ep, 8, 2000),
        "the registration went deaf after one event",
    );
    assert_eq!(token, TOKEN);
    let n = server.read(&mut buf).expect("server read again");
    assert_eq!(&buf[..n], b"second");
    println!("second write reported too: the registration survived");

    // MOD to a different token, so the update path is covered.
    const TOKEN2: u64 = 0xf00d;
    ctl(ep, libc::EPOLL_CTL_MOD, sfd, READ, TOKEN2);
    client.write_all(b"third").expect("client write third");
    let (_, token) = only(&wait(ep, 8, 2000), "third write");
    assert_eq!(token, TOKEN2, "EPOLL_CTL_MOD did not take");
    let n = server.read(&mut buf).expect("server read third");
    assert_eq!(&buf[..n], b"third");
    println!("EPOLL_CTL_MOD swapped the token");

    // Peer hangup, the other thing mio leans on.
    drop(client);
    let (events, _) = only(&wait(ep, 8, 2000), "hangup was not reported");
    let hup = events & (libc::EPOLLHUP as u32 | libc::EPOLLRDHUP as u32 | libc::EPOLLIN as u32);
    assert_ne!(
        hup, 0,
        "hangup reported without any of HUP/RDHUP/IN: {events:#x}"
    );
    println!("peer hangup reported: events={events:#x}");

    // DEL, after which the set is silent again even though the fd still has
    // its hangup pending.
    ctl(ep, libc::EPOLL_CTL_DEL, sfd, 0, 0);
    assert!(
        wait(ep, 8, 50).is_empty(),
        "a deregistered fd still reports"
    );
    println!("EPOLL_CTL_DEL went quiet");

    // Two fds at once, so the mapping from fd to token is exercised with
    // more than one entry in the set.
    let a = TcpListener::bind("127.0.0.1:0").expect("bind a");
    let b = TcpListener::bind("127.0.0.1:0").expect("bind b");
    let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
    ctl(
        ep,
        libc::EPOLL_CTL_ADD,
        a.as_raw_fd(),
        libc::EPOLLIN as u32,
        0xa,
    );
    ctl(
        ep,
        libc::EPOLL_CTL_ADD,
        b.as_raw_fd(),
        libc::EPOLLIN as u32,
        0xb,
    );
    let _ca = TcpStream::connect(a_addr).expect("connect a");
    let _cb = TcpStream::connect(b_addr).expect("connect b");
    let evs = wait(ep, 8, 2000);
    let mut tokens: Vec<u64> = evs
        .iter()
        .map(|e| {
            let t = e.u64;
            t
        })
        .collect();
    tokens.sort_unstable();
    assert_eq!(
        tokens,
        vec![0xa, 0xb],
        "two pending listeners came back as {tokens:?}"
    );
    println!("two fds reported independently, tokens {tokens:?}");
    ctl(ep, libc::EPOLL_CTL_DEL, a.as_raw_fd(), 0, 0);
    ctl(ep, libc::EPOLL_CTL_DEL, b.as_raw_fd(), 0, 0);

    // An idle socket registered EPOLLOUT|EPOLLET reports once and then stays
    // quiet. mio registers every stream this way and tokio clears the
    // readiness only on WouldBlock, so a second report here means a reactor
    // that never sleeps.
    let (mut w, _r) = {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind w");
        let c = TcpStream::connect(l.local_addr().unwrap()).expect("connect w");
        let (s, _) = l.accept().expect("accept w");
        (c, s)
    };
    w.set_nonblocking(true).expect("set_nonblocking w");
    const OUT: u32 = libc::EPOLLOUT as u32 | libc::EPOLLET as u32;
    ctl(ep, libc::EPOLL_CTL_ADD, w.as_raw_fd(), OUT, 0x0a7);
    let (events, token) = only(&wait(ep, 8, 2000), "idle socket never reported writable");
    assert_eq!(token, 0x0a7);
    assert_ne!(
        events & libc::EPOLLOUT as u32,
        0,
        "writable but not for writing: {events:#x}"
    );
    assert!(
        wait(ep, 8, 100).is_empty(),
        "EPOLLOUT|EPOLLET reported an idle socket twice"
    );
    // A write may cost one more report where the host's edge is the send
    // buffer changing (kqueue does that; Linux does not), but never a
    // stream of them.
    w.write_all(b"ping").expect("write w");
    let after_write = wait(ep, 8, 100).len();
    assert!(
        after_write <= 1,
        "a write on an idle socket produced {after_write} reports"
    );
    assert!(
        wait(ep, 8, 100).is_empty(),
        "the socket kept reporting writable after a write"
    );
    println!(
        "idle EPOLLOUT|EPOLLET reported once and then stayed quiet ({after_write} after a write)"
    );

    // An eventfd written from another thread wakes a wait, which is how
    // mio's waker reaches the reactor.
    let efd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    assert!(efd >= 0, "eventfd: {}", std::io::Error::last_os_error());
    ctl(
        ep,
        libc::EPOLL_CTL_ADD,
        efd,
        libc::EPOLLIN as u32 | libc::EPOLLET as u32,
        0xe0,
    );
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let one: u64 = 1;
        assert_eq!(
            unsafe { libc::write(efd, &one as *const u64 as *const _, 8) },
            8
        );
    });
    let started = std::time::Instant::now();
    let (_, token) = only(&wait(ep, 8, 5000), "the eventfd write never woke the wait");
    t.join().unwrap();
    assert_eq!(token, 0xe0);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "woke late: {:?}",
        started.elapsed()
    );
    let mut got: u64 = 0;
    assert_eq!(
        unsafe { libc::read(efd, &mut got as *mut u64 as *mut _, 8) },
        8
    );
    assert_eq!(got, 1);
    assert!(
        wait(ep, 8, 50).is_empty(),
        "a drained eventfd still reports"
    );
    println!(
        "eventfd written from another thread woke the wait after {:?}",
        started.elapsed()
    );

    // Closing a registered fd removes it, with no EPOLL_CTL_DEL. tokio drops
    // sockets exactly like this.
    let l = TcpListener::bind("127.0.0.1:0").expect("bind c");
    let c = TcpStream::connect(l.local_addr().unwrap()).expect("connect c");
    let (mut s, _) = l.accept().expect("accept c");
    ctl(ep, libc::EPOLL_CTL_ADD, s.as_raw_fd(), READ, 0xc10);
    s.write_all(b"bye").expect("write c");
    drop(c);
    drop(s);
    assert!(wait(ep, 8, 100).is_empty(), "a closed fd still reports");
    println!("closed fd left the set on its own");

    assert_eq!(unsafe { libc::close(efd) }, 0, "close(efd)");
    assert_eq!(unsafe { libc::close(ep) }, 0, "close(epfd)");
    assert_eq!(
        unsafe { libc::epoll_wait(ep, std::ptr::null_mut(), 1, 0) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBADF)
    );
    println!("closed epoll fd answers EBADF");
    println!("\nepoll probe ok");
}
