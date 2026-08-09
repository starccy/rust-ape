//! UDP on the tokio runtime, plus the async name lookup that sits in front of it.

use std::time::Duration;

use tokio::net::{TcpListener, UdpSocket, lookup_host};

const CLIENTS: usize = 16;
const ROUNDS: usize = 8;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        datagrams().await;
        connected().await;
        names().await;
    });

    println!("\ntokio udp ok");
}

/// One echo server against sixteen clients. Each payload names its sender, so a
/// datagram delivered to the wrong socket would be caught rather than counted.
async fn datagrams() {
    let server = UdpSocket::bind("127.0.0.1:0").await.expect("bind server");
    let addr = server.local_addr().expect("local_addr");
    println!("udp server on {addr}");

    let echo = tokio::spawn(async move {
        let mut buf = [0u8; 128];
        for _ in 0..CLIENTS * ROUNDS {
            let (n, from) = server.recv_from(&mut buf).await.expect("recv_from");
            server.send_to(&buf[..n], from).await.expect("send_to");
        }
    });

    let clients: Vec<_> = (0..CLIENTS)
        .map(|id| {
            tokio::spawn(async move {
                let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind client");
                let mut buf = [0u8; 128];
                for round in 0..ROUNDS {
                    let msg = format!("c{id}r{round}");
                    sock.send_to(msg.as_bytes(), addr).await.expect("send_to");
                    let (n, from) = sock.recv_from(&mut buf).await.expect("recv_from");
                    assert_eq!(from, addr, "reply came from {from}, not the server");
                    assert_eq!(&buf[..n], msg.as_bytes(), "client {id} got someone else's datagram");
                }
            })
        })
        .collect();

    for c in clients {
        c.await.expect("client");
    }
    echo.await.expect("echo server");
    println!("{} datagrams echoed across {CLIENTS} sockets", CLIENTS * ROUNDS);
}

/// A connected UDP socket uses send/recv rather than send_to/recv_from, which is
/// a different path in the driver.
async fn connected() {
    let server = UdpSocket::bind("127.0.0.1:0").await.expect("bind server");
    let server_addr = server.local_addr().expect("local_addr");
    let client = UdpSocket::bind("127.0.0.1:0").await.expect("bind client");
    client.connect(server_addr).await.expect("connect");
    let client_addr = client.local_addr().expect("client local_addr");
    server.connect(client_addr).await.expect("server connect");

    let echo = tokio::spawn(async move {
        let mut buf = [0u8; 64];
        let n = server.recv(&mut buf).await.expect("recv");
        server.send(&buf[..n]).await.expect("send");
    });

    client.send(b"connected").await.expect("send");
    let mut buf = [0u8; 64];
    let n = client.recv(&mut buf).await.expect("recv");
    assert_eq!(&buf[..n], b"connected");
    echo.await.expect("echo");

    // Nothing else is coming, so this has to time out rather than return.
    let r = tokio::time::timeout(Duration::from_millis(100), client.recv(&mut buf)).await;
    assert!(r.is_err(), "recv returned on an idle socket");
    println!("connected socket: send/recv roundtrip, idle recv timed out");
}

/// lookup_host runs getaddrinfo on the blocking pool. Only loopback is used, so
/// this doesn't depend on the machine having DNS.
async fn names() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("local_addr").port();

    let resolved: Vec<_> = lookup_host(("localhost", port)).await.expect("lookup_host").collect();
    assert!(!resolved.is_empty(), "localhost resolved to nothing");
    assert!(resolved.iter().all(|a| a.ip().is_loopback()), "localhost gave {resolved:?}");
    assert!(resolved.iter().all(|a| a.port() == port));

    // Connecting by name goes through the same resolver.
    let accepted = tokio::spawn(async move { listener.accept().await.expect("accept") });
    let sock = tokio::net::TcpStream::connect(("localhost", port)).await.expect("connect by name");
    assert!(sock.peer_addr().expect("peer_addr").ip().is_loopback());
    accepted.await.expect("accept task");

    println!("lookup_host: localhost -> {} address(es), connect by name worked", resolved.len());
}
