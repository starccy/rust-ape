//! UDP server and client on loopback, plus the options that only exist on a
//! datagram socket.

use std::mem;
use std::net::{Ipv4Addr, UdpSocket};
use std::os::fd::AsRawFd;
use std::thread;
use std::time::Duration;

const DATAGRAMS: [&str; 3] = ["one", "two", "three"];

fn get_int(fd: i32, level: i32, name: i32) -> i32 {
    let mut val: i32 = -1;
    let mut len = mem::size_of::<i32>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(fd, level, name, &mut val as *mut _ as *mut libc::c_void, &mut len)
    };
    assert_eq!(rc, 0, "getsockopt({name}) failed: {}", std::io::Error::last_os_error());
    val
}

fn main() {
    let server = UdpSocket::bind("127.0.0.1:0").expect("bind server");
    let server_addr = server.local_addr().expect("server local_addr");
    println!("server on {server_addr}");

    let echo = thread::spawn(move || {
        let mut buf = [0u8; 512];
        for _ in 0..DATAGRAMS.len() {
            let (n, from) = server.recv_from(&mut buf).expect("recv_from");
            server.send_to(&buf[..n], from).expect("send_to");
        }
    });

    let client = UdpSocket::bind("127.0.0.1:0").expect("bind client");
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set_read_timeout");

    for msg in DATAGRAMS {
        client.send_to(msg.as_bytes(), server_addr).expect("client send_to");

        let mut buf = [0u8; 512];
        let (n, from) = client.recv_from(&mut buf).expect("client recv_from");
        assert_eq!(&buf[..n], msg.as_bytes(), "datagram came back changed");
        assert_eq!(from, server_addr, "datagram came back from the wrong peer");
        println!("{msg:?} <-> {from}");
    }

    echo.join().expect("echo thread");

    // std has getters for all of these, so every line below is a
    // setsockopt/getsockopt pair crossing the shim in both directions.
    assert_eq!(
        get_int(client.as_raw_fd(), libc::SOL_SOCKET, libc::SO_TYPE),
        libc::SOCK_DGRAM,
        "SO_TYPE came back in the host's coding, not musl's"
    );

    client.set_broadcast(true).expect("set_broadcast");
    assert!(client.broadcast().expect("broadcast"), "SO_BROADCAST did not stick");
    client.set_broadcast(false).expect("clear SO_BROADCAST");

    client.set_ttl(7).expect("set_ttl");
    assert_eq!(client.ttl().expect("ttl"), 7, "IP_TTL did not stick");

    client.set_multicast_loop_v4(false).expect("set_multicast_loop_v4");
    assert!(!client.multicast_loop_v4().expect("multicast_loop_v4"), "IP_MULTICAST_LOOP did not stick");
    client.set_multicast_loop_v4(true).expect("restore IP_MULTICAST_LOOP");

    client.set_multicast_ttl_v4(4).expect("set_multicast_ttl_v4");
    assert_eq!(client.multicast_ttl_v4().expect("multicast_ttl_v4"), 4, "IP_MULTICAST_TTL did not stick");
    println!("SO_TYPE, SO_BROADCAST, IP_TTL and the IP_MULTICAST_* pair all round-tripped");

    // IP_ADD_MEMBERSHIP carries an ip_mreq rather than an int, so its payload
    // crosses the shim unrepacked. Whether the join succeeds at all is the
    // machine's business though: a CI runner need not have an interface that
    // will carry the group. Report that case rather than fail on it.
    let group = Ipv4Addr::new(239, 255, 42, 99);
    match client.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED) {
        Ok(()) => {
            client
                .leave_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)
                .expect("leave_multicast_v4");
            println!("joined and left {group}, IP_ADD_MEMBERSHIP/IP_DROP_MEMBERSHIP ok");
        }
        Err(e) => println!("could not join {group}, no usable interface here: {e}"),
    }

    println!("\nudp echo ok: {} datagrams round-tripped", DATAGRAMS.len());
}
