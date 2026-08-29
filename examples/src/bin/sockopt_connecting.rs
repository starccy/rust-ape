//! Socket options set while a nonblocking connect is still in flight.
//!
//! Linux accepts them immediately; the option takes effect once the
//! connection is up. The pattern is common (Python's socket module does it
//! for every connect with a timeout), so the shim must make NT behave the
//! same way.

use std::io::{Read, Write};
use std::mem;
use std::net::TcpListener;
use std::os::fd::FromRawFd;

fn check(r: i32, what: &str) -> i32 {
    if r < 0 {
        panic!("{what}: {}", std::io::Error::last_os_error());
    }
    r
}

fn main() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let port = addr.port();

    unsafe {
        let fd = check(
            libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_NONBLOCK, 0),
            "socket",
        );
        let sin = libc::sockaddr_in {
            sin_family: libc::AF_INET as _,
            sin_port: port.to_be(),
            sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes([127, 0, 0, 1]) },
            sin_zero: [0; 8],
        };
        let r = libc::connect(
            fd,
            &sin as *const _ as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_in>() as _,
        );
        let err = std::io::Error::last_os_error();
        let in_progress = r == -1 && err.raw_os_error() == Some(libc::EINPROGRESS);
        assert!(r == 0 || in_progress, "connect: {err}");
        println!("connect: {}", if in_progress { "in progress" } else { "done" });

        // The point of the test: these must succeed right away.
        let one: libc::c_int = 1;
        for (level, name, label) in [
            (libc::IPPROTO_TCP, libc::TCP_NODELAY, "TCP_NODELAY"),
            (libc::SOL_SOCKET, libc::SO_KEEPALIVE, "SO_KEEPALIVE"),
        ] {
            check(
                libc::setsockopt(fd, level, name, &one as *const _ as *const _, 4),
                label,
            );
            let mut got: libc::c_int = 0;
            let mut len: libc::socklen_t = 4;
            check(
                libc::getsockopt(fd, level, name, &mut got as *mut _ as *mut _, &mut len),
                label,
            );
            assert_eq!(got, 1, "{label} reads back as set");
            println!("{label}: set and read back");
        }

        let mut p = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
        check(libc::poll(&mut p, 1, 5000), "poll");
        assert!(p.revents & libc::POLLOUT != 0, "connect did not complete");
        let mut soerr: libc::c_int = -1;
        let mut len: libc::socklen_t = 4;
        check(
            libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_ERROR, &mut soerr as *mut _ as *mut _, &mut len),
            "SO_ERROR",
        );
        assert_eq!(soerr, 0, "connect failed");

        let mut stream = std::net::TcpStream::from_raw_fd(fd);
        stream.set_nonblocking(false).expect("blocking");
        stream.write_all(b"hello").expect("write");

        // With the connection up, the options must be really applied.
        let mut got: libc::c_int = 0;
        let mut len: libc::socklen_t = 4;
        check(
            libc::getsockopt(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY, &mut got as *mut _ as *mut _, &mut len),
            "TCP_NODELAY after connect",
        );
        assert_eq!(got, 1, "TCP_NODELAY stuck after the handshake");

        let (mut peer, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 5];
        peer.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"hello");
        println!("echo through: ok");
    }
}
