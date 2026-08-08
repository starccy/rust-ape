//! Blocking TCP echo server and client on one loopback socket pair.
//!
//! Touches the socket options std has to translate per host: SOCK_CLOEXEC on
//! creation, TCP_NODELAY and SO_RCVTIMEO through set/getsockopt, O_NONBLOCK via
//! fcntl, and MSG_NOSIGNAL on send.

use std::io::{Read, Write};
use std::mem;
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::thread;
use std::time::Duration;

const MESSAGES: [&str; 4] = ["hello", "cosmopolitan", "actually portable", "bye"];

fn get_int(fd: i32, level: i32, name: i32) -> i32 {
    let mut val: i32 = -1;
    let mut len = mem::size_of::<i32>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(fd, level, name, &mut val as *mut _ as *mut libc::c_void, &mut len)
    };
    assert_eq!(rc, 0, "getsockopt({name}) failed: {}", std::io::Error::last_os_error());
    assert_eq!(len as usize, mem::size_of::<i32>(), "getsockopt({name}) returned {len} bytes");
    val
}

/// Not every option is queryable on every host. XNU has no getsockopt answer
/// for SO_ACCEPTCONN and returns ENOPROTOOPT, so that one is reported rather
/// than asserted.
fn try_get_int(fd: i32, level: i32, name: i32) -> Option<i32> {
    let mut val: i32 = -1;
    let mut len = mem::size_of::<i32>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(fd, level, name, &mut val as *mut _ as *mut libc::c_void, &mut len)
    };
    (rc == 0).then_some(val)
}

fn set_int(fd: i32, level: i32, name: i32, val: i32) {
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            &val as *const _ as *const libc::c_void,
            mem::size_of::<i32>() as libc::socklen_t,
        )
    };
    assert_eq!(rc, 0, "setsockopt({name}) failed: {}", std::io::Error::last_os_error());
}

fn main() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    println!("listening on {addr}");

    // A listening socket, before it goes off to the server thread.
    let lfd = listener.as_raw_fd();
    assert_eq!(
        get_int(lfd, libc::SOL_SOCKET, libc::SO_TYPE),
        libc::SOCK_STREAM,
        "SO_TYPE came back in the host's coding, not musl's"
    );
    match try_get_int(lfd, libc::SOL_SOCKET, libc::SO_ACCEPTCONN) {
        Some(v) => assert_ne!(v, 0, "a listening socket says it isn't listening"),
        None => println!("SO_ACCEPTCONN not queryable here: {}", std::io::Error::last_os_error()),
    }
    set_int(lfd, libc::SOL_SOCKET, libc::SO_REUSEADDR, 1);
    assert_ne!(get_int(lfd, libc::SOL_SOCKET, libc::SO_REUSEADDR), 0, "SO_REUSEADDR did not stick");
    println!("listener: SO_TYPE and SO_REUSEADDR ok");

    let server = thread::spawn(move || {
        let (mut sock, peer) = listener.accept().expect("accept");
        println!("accepted {peer}");
        // Read until the client is done writing, echoing as we go.
        let mut buf = [0u8; 256];
        let mut echoed = 0usize;
        loop {
            match sock.read(&mut buf).expect("server read") {
                0 => break,
                n => {
                    sock.write_all(&buf[..n]).expect("server write");
                    echoed += n;
                }
            }
        }
        echoed
    });

    let mut client = TcpStream::connect(addr).expect("connect");

    // TCP_NODELAY: setsockopt then getsockopt, so both directions are exercised.
    client.set_nodelay(true).expect("set_nodelay");
    assert!(client.nodelay().expect("nodelay"), "TCP_NODELAY did not stick");

    // SO_RCVTIMEO. Generous, since it's here to be set, not to fire.
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set_read_timeout");

    // O_NONBLOCK on and back off, through fcntl.
    client.set_nonblocking(true).expect("set_nonblocking(true)");
    client.set_nonblocking(false).expect("set_nonblocking(false)");

    let mut sent = 0usize;
    for msg in MESSAGES {
        client.write_all(msg.as_bytes()).expect("client write");
        sent += msg.len();

        let mut back = vec![0u8; msg.len()];
        client.read_exact(&mut back).expect("client read");
        assert_eq!(back, msg.as_bytes(), "echo differs from what we sent");
        println!("echoed {:?}", msg);
    }

    // The connected end, after a clean exchange.
    let cfd = client.as_raw_fd();
    assert_eq!(get_int(cfd, libc::SOL_SOCKET, libc::SO_TYPE), libc::SOCK_STREAM, "SO_TYPE");
    assert_eq!(get_int(cfd, libc::SOL_SOCKET, libc::SO_ERROR), 0, "a healthy socket reports an error");
    if let Some(v) = try_get_int(cfd, libc::SOL_SOCKET, libc::SO_ACCEPTCONN) {
        assert_eq!(v, 0, "a connected socket claims to be listening");
    }
    set_int(cfd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
    assert_ne!(get_int(cfd, libc::SOL_SOCKET, libc::SO_KEEPALIVE), 0, "SO_KEEPALIVE did not stick");
    let sndbuf = get_int(cfd, libc::SOL_SOCKET, libc::SO_SNDBUF);
    let rcvbuf = get_int(cfd, libc::SOL_SOCKET, libc::SO_RCVBUF);
    assert!(sndbuf > 0 && rcvbuf > 0, "buffer sizes came back as {sndbuf}/{rcvbuf}");
    println!("connected: SO_ERROR clear, SO_KEEPALIVE set, buffers {sndbuf}/{rcvbuf}");

    // SO_LINGER carries a struct rather than an int, so the payload crosses
    // the shim unrepacked and the two layouts have to already agree.
    unsafe {
        let want = libc::linger { l_onoff: 1, l_linger: 3 };
        assert_eq!(
            libc::setsockopt(
                cfd,
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                &want as *const _ as *const libc::c_void,
                mem::size_of::<libc::linger>() as libc::socklen_t,
            ),
            0,
            "setsockopt(SO_LINGER) failed: {}",
            std::io::Error::last_os_error()
        );
        let mut got: libc::linger = mem::zeroed();
        let mut len = mem::size_of::<libc::linger>() as libc::socklen_t;
        assert_eq!(
            libc::getsockopt(
                cfd,
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                &mut got as *mut _ as *mut libc::c_void,
                &mut len,
            ),
            0,
            "getsockopt(SO_LINGER)"
        );
        assert_ne!(got.l_onoff, 0, "SO_LINGER l_onoff did not stick");
        assert_eq!(got.l_linger, 3, "SO_LINGER l_linger came back as {}", got.l_linger);
        println!("SO_LINGER struct round-trip ok");
        // Back off, so the close below doesn't sit and wait.
        let off = libc::linger { l_onoff: 0, l_linger: 0 };
        libc::setsockopt(
            cfd,
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            &off as *const _ as *const libc::c_void,
            mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }

    // Half-close so the server's read returns 0 and its loop ends.
    client.shutdown(std::net::Shutdown::Write).expect("shutdown");
    let echoed = server.join().expect("server thread");
    assert_eq!(echoed, sent, "server echoed {echoed} bytes, we sent {sent}");

    println!("\ntcp echo ok: {sent} bytes round-tripped");
}
