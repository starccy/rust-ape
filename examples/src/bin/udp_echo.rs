//! demo for test UDP server/client

use std::net::UdpSocket;
use std::thread;
use std::time::Duration;

const DATAGRAMS: [&str; 3] = ["one", "two", "three"];

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
    println!("\nudp echo ok: {} datagrams round-tripped", DATAGRAMS.len());
}
