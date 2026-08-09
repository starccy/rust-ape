//! The epoll shim (shim/epoll.c), exercised directly rather than through mio.
//!
//! cosmo deleted epoll in 2024, so these three entry points are the shim's
//! own: raw syscalls on Linux, an emulation over cosmo's poll() elsewhere.
//! Everything here is what mio's selector actually does to them, in the order
//! it does it, so a break shows up as a failed assert rather than as an async
//! runtime that quietly stops waking up.
//!
//! EPOLLET is asked for the way mio asks for it, but nothing below depends on
//! edge semantics: the emulation is level-triggered on purpose and reporting
//! an event twice is allowed. Reporting it zero times is not.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;

/// The wait side, with a generous timeout, returning what came back.
fn wait(ep: i32, max: usize, timeout_ms: i32) -> Vec<libc::epoll_event> {
    let mut evs: Vec<libc::epoll_event> =
        (0..max).map(|_| libc::epoll_event { events: 0, u64: 0 }).collect();
    let n = unsafe { libc::epoll_wait(ep, evs.as_mut_ptr(), max as i32, timeout_ms) };
    assert!(n >= 0, "epoll_wait failed: {}", std::io::Error::last_os_error());
    evs.truncate(n as usize);
    evs
}

fn ctl(ep: i32, op: i32, fd: i32, events: u32, token: u64) {
    let mut ev = libc::epoll_event { events, u64: token };
    let rc = unsafe { libc::epoll_ctl(ep, op, fd, &mut ev) };
    assert_eq!(rc, 0, "epoll_ctl(op={op}, fd={fd}) failed: {}", std::io::Error::last_os_error());
}

/// `epoll_event` is packed on x86_64, so taking a reference to a field is
/// undefined behavior and every assert below would do exactly that. Copy the
/// two fields out by value first.
fn only(evs: &[libc::epoll_event], what: &str) -> (u32, u64) {
    assert_eq!(evs.len(), 1, "{what}: expected exactly one ready fd, got {}", evs.len());
    (evs[0].events, evs[0].u64)
}

fn main() {
    // The struct crosses the shim boundary unrepacked, so the two sides have
    // to already agree. It is packed on x86_64 and naturally aligned on
    // aarch64, which is exactly why this is worth pinning.
    #[cfg(target_arch = "x86_64")]
    assert_eq!(std::mem::size_of::<libc::epoll_event>(), 12, "epoll_event is not packed");
    #[cfg(target_arch = "aarch64")]
    assert_eq!(std::mem::size_of::<libc::epoll_event>(), 16, "epoll_event grew padding");

    let ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    assert!(ep >= 0, "epoll_create1 failed: {}", std::io::Error::last_os_error());
    println!("epoll_create1 -> fd {ep}");

    // An empty set must time out rather than report anything.
    let evs = wait(ep, 8, 50);
    assert!(evs.is_empty(), "an empty epoll reported {} events", evs.len());
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
    assert!(wait(ep, 8, 50).is_empty(), "reported readable before anything was sent");

    client.write_all(b"first").expect("client write");
    let (events, token) = only(&wait(ep, 8, 2000), "first write");
    assert_eq!(token, TOKEN, "the token came back changed");
    assert_ne!(events & libc::EPOLLIN as u32, 0, "ready but not for reading");
    println!("first write reported: events={events:#x} token={token:#x}");

    let mut buf = [0u8; 32];
    let n = server.read(&mut buf).expect("server read");
    assert_eq!(&buf[..n], b"first");

    // The part every poll-backed selector gets wrong. A registration is not
    // consumed by being reported; the second write has to arrive too, with no
    // re-registration in between.
    client.write_all(b"second").expect("client write again");
    let (_, token) = only(&wait(ep, 8, 2000), "the registration went deaf after one event");
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
    assert_ne!(hup, 0, "hangup reported without any of HUP/RDHUP/IN: {events:#x}");
    println!("peer hangup reported: events={events:#x}");

    // DEL, after which the set is silent again even though the fd still has
    // its hangup pending.
    ctl(ep, libc::EPOLL_CTL_DEL, sfd, 0, 0);
    assert!(wait(ep, 8, 50).is_empty(), "a deregistered fd still reports");
    println!("EPOLL_CTL_DEL went quiet");

    // Two fds at once, so the mapping from fd to token is exercised with
    // more than one entry in the set.
    let a = TcpListener::bind("127.0.0.1:0").expect("bind a");
    let b = TcpListener::bind("127.0.0.1:0").expect("bind b");
    let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
    ctl(ep, libc::EPOLL_CTL_ADD, a.as_raw_fd(), libc::EPOLLIN as u32, 0xa);
    ctl(ep, libc::EPOLL_CTL_ADD, b.as_raw_fd(), libc::EPOLLIN as u32, 0xb);
    let _ca = TcpStream::connect(a_addr).expect("connect a");
    let _cb = TcpStream::connect(b_addr).expect("connect b");
    let evs = wait(ep, 8, 2000);
    let mut tokens: Vec<u64> = evs.iter().map(|e| { let t = e.u64; t }).collect();
    tokens.sort_unstable();
    assert_eq!(tokens, vec![0xa, 0xb], "two pending listeners came back as {tokens:?}");
    println!("two fds reported independently, tokens {tokens:?}");

    assert_eq!(unsafe { libc::close(ep) }, 0, "close(epfd)");
    println!("\nepoll probe ok");
}
