//! IPv6 through the AF shim. AF_INET6 is a runtime constant under cosmo (10
//! on Linux, 23 on Windows, 30 on XNU), while the Rust world bakes musl's 10
//! into both the socket() domain argument and the sin6_family field of
//! sockaddr_in6. The struct layout itself matches, so the shim only rewrites
//! the family value, forward on the way in and back on the way out. Before
//! the shim this failed at the first bind off Linux.

use std::io::{Read, Write};
use std::net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};

fn main() {
    // TCP over ::1, with the address observed from both ends.
    let listener = TcpListener::bind("[::1]:0").expect("bind [::1]:0");
    let addr = listener.local_addr().expect("local_addr");
    assert!(addr.is_ipv6(), "listener local_addr came back non-v6: {addr}");

    let mut tx = TcpStream::connect(addr).expect("connect over v6");
    let (mut rx, peer) = listener.accept().expect("accept over v6");
    assert!(peer.is_ipv6(), "accept's peer address came back non-v6: {peer}");
    assert!(tx.local_addr().expect("tx local_addr").is_ipv6());

    tx.write_all(b"over six").expect("write");
    let mut buf = [0u8; 8];
    rx.read_exact(&mut buf).expect("read");
    assert_eq!(&buf, b"over six");
    println!("tcp over [::1] ok ({addr} <- {peer})");

    // UDP: recv_from's source address is a reverse-rewritten sockaddr_in6.
    let a = UdpSocket::bind("[::1]:0").expect("udp bind a");
    let b = UdpSocket::bind("[::1]:0").expect("udp bind b");
    let b_addr = b.local_addr().expect("b local_addr");
    a.send_to(b"ping6", b_addr).expect("send_to");
    let mut buf = [0u8; 5];
    let (n, from) = b.recv_from(&mut buf).expect("recv_from");
    assert_eq!((n, &buf), (5, b"ping6"));
    assert_eq!(from.ip(), std::net::IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(from.port(), a.local_addr().expect("a local_addr").port());
    println!("udp over [::1] ok (reply address {from})");

    // getaddrinfo: the result chain's family fields come back musl-coded, so
    // std can recognize the entries at all. v6 presence depends on the host's
    // resolver config; finding the v4 loopback proves the chain parses.
    let resolved: Vec<SocketAddr> = std::net::ToSocketAddrs::to_socket_addrs("localhost:80")
        .expect("resolve localhost")
        .collect();
    assert!(!resolved.is_empty(), "localhost resolved to nothing");
    println!(
        "getaddrinfo(localhost) -> {} entries, v4:{} v6:{}",
        resolved.len(),
        resolved.iter().filter(|a| a.is_ipv4()).count(),
        resolved.iter().filter(|a| a.is_ipv6()).count(),
    );

    // The raw libc route, the way third-party crates do it: socket(AF_INET6)
    // must open, and getsockname must hand musl's 10 back in sin6_family.
    unsafe {
        let fd = libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0);
        assert!(fd >= 0, "socket(AF_INET6) failed");
        let mut sa: libc::sockaddr_in6 = std::mem::zeroed();
        sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
        sa.sin6_addr.s6_addr[15] = 1; // ::1
        assert_eq!(
            libc::bind(fd, &sa as *const _ as *const libc::sockaddr,
                       std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t),
            0,
            "raw bind to ::1 failed"
        );
        let mut out: libc::sockaddr_in6 = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t;
        assert_eq!(
            libc::getsockname(fd, &mut out as *mut _ as *mut libc::sockaddr, &mut len),
            0
        );
        assert_eq!(out.sin6_family as i32, libc::AF_INET6,
            "getsockname's family not rewritten back to musl coding");
        assert_ne!(out.sin6_port, 0);
        libc::close(fd);
        println!("raw socket/bind/getsockname AF_INET6 round-trip ok");
    }

    println!("\nipv6 ok");
}
