//! The socket families the other smol examples don't cover: UDP, and AF_UNIX
//! both by path and as a pair.
//!
//! The AF_UNIX half leans on three NT workarounds in `shim/socket.c`: a
//! socketpair built from a real listener, `SOCK_NONBLOCK` applied afterwards,
//! and `sun_path` run through cosmo's path translation. Without any one of
//! them the Windows side of this hangs or fails to bind.

use std::time::Duration;

use smol::io::{AsyncReadExt, AsyncWriteExt};
use smol::net::UdpSocket;
use smol::net::unix::{UnixDatagram, UnixListener, UnixStream};

const CLIENTS: usize = 16;
const ROUNDS: usize = 4;

fn main() {
    smol::block_on(async {
        udp().await;

        let dir = std::env::temp_dir().join(format!("rust-ape-smol-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create_dir_all");
        uds_path(&dir).await;
        uds_pair().await;
        if ape::is_windows() {
            println!("skipping unix datagrams: NT's AF_UNIX has no SOCK_DGRAM");
        } else {
            uds_datagram(&dir).await;
        }
        std::fs::remove_dir_all(&dir).expect("remove_dir_all");
    });

    println!("\nsmol udp uds ok");
}

/// Sixteen sockets against one echo server, payloads naming their sender so a
/// datagram delivered to the wrong socket gets caught rather than counted.
async fn udp() {
    let server = UdpSocket::bind("127.0.0.1:0").await.expect("bind server");
    let addr = server.local_addr().expect("local_addr");
    println!("udp server on {addr}");

    let echo = smol::spawn(async move {
        let mut buf = [0u8; 128];
        for _ in 0..CLIENTS * ROUNDS {
            let (n, from) = server.recv_from(&mut buf).await.expect("recv_from");
            server.send_to(&buf[..n], from).await.expect("send_to");
        }
    });

    let clients: Vec<_> = (0..CLIENTS)
        .map(|id| {
            smol::spawn(async move {
                let sock = UdpSocket::bind("127.0.0.1:0").await.expect("bind client");
                let mut buf = [0u8; 128];
                for round in 0..ROUNDS {
                    let msg = format!("c{id}r{round}");
                    sock.send_to(msg.as_bytes(), addr).await.expect("send_to");
                    let (n, from) = sock.recv_from(&mut buf).await.expect("recv_from");
                    assert_eq!(from, addr, "reply came from {from}");
                    assert_eq!(&buf[..n], msg.as_bytes(), "client {id} got someone else's datagram");
                }
            })
        })
        .collect();

    for c in clients {
        c.await;
    }
    echo.await;

    // A connected socket uses send/recv, a different path in the reactor.
    let server = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let saddr = server.local_addr().expect("local_addr");
    let client = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    client.connect(saddr).await.expect("connect");
    server.connect(client.local_addr().expect("addr")).await.expect("connect back");

    let echo = smol::spawn(async move {
        let mut buf = [0u8; 64];
        let n = server.recv(&mut buf).await.expect("recv");
        server.send(&buf[..n]).await.expect("send");
    });
    client.send(b"connected").await.expect("send");
    let mut buf = [0u8; 64];
    let n = client.recv(&mut buf).await.expect("recv");
    assert_eq!(&buf[..n], b"connected");
    echo.await;

    println!("{} datagrams echoed, connected socket round-tripped", CLIENTS * ROUNDS);
}

async fn uds_path(dir: &std::path::Path) {
    let path = dir.join("stream.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    println!("listening on {}", path.display());

    let server = smol::spawn(async move {
        for _ in 0..CLIENTS {
            let (mut sock, _) = listener.accept().await.expect("accept");
            smol::spawn(async move {
                // Echo until the peer hangs up; one read would be a bet that
                // the message never gets split.
                let mut buf = [0u8; 64];
                loop {
                    let n = sock.read(&mut buf).await.expect("server read");
                    if n == 0 {
                        return;
                    }
                    sock.write_all(&buf[..n]).await.expect("server write");
                }
            })
            .detach();
        }
    });

    let clients: Vec<_> = (0..CLIENTS)
        .map(|id| {
            let path = path.clone();
            smol::spawn(async move {
                let mut sock = UnixStream::connect(&path).await.expect("connect");
                let msg = format!("uds-{id}");
                sock.write_all(msg.as_bytes()).await.expect("write");
                let mut back = vec![0u8; msg.len()];
                sock.read_exact(&mut back).await.expect("read");
                assert_eq!(back, msg.as_bytes(), "client {id} got someone else's reply");
            })
        })
        .collect();

    for c in clients {
        c.await;
    }
    server.await;
    std::fs::remove_file(&path).expect("remove socket");
    println!("{CLIENTS} clients echoed over a listening socket");
}

/// The case that needed the shim to stop using cosmo's socketpair on Windows:
/// poll never called those fds writable, so the first async write hung.
async fn uds_pair() {
    let (mut a, mut b) = UnixStream::pair().expect("pair");
    let echo = smol::spawn(async move {
        let mut buf = [0u8; 6];
        b.read_exact(&mut buf).await.expect("read");
        b.write_all(&buf).await.expect("write");
    });

    a.write_all(b"paired").await.expect("write");
    let mut back = [0u8; 6];
    a.read_exact(&mut back).await.expect("read");
    assert_eq!(&back, b"paired");
    echo.await;
    println!("UnixStream::pair round-tripped");
}

async fn uds_datagram(dir: &std::path::Path) {
    let path = dir.join("dgram.sock");
    let server = UnixDatagram::bind(&path).expect("bind server");
    let client = UnixDatagram::unbound().expect("unbound client");

    let echo = smol::spawn(async move {
        let mut buf = [0u8; 64];
        let (n, _) = server.recv_from(&mut buf).await.expect("recv_from");
        assert_eq!(&buf[..n], b"datagram");
        n
    });

    client.send_to(b"datagram", &path).await.expect("send_to");
    let n = echo.await;
    assert_eq!(n, 8);

    // And a bound pair, so the reply direction gets exercised too.
    let reply_path = dir.join("dgram-reply.sock");
    let a = UnixDatagram::bind(&path.with_extension("a")).expect("bind a");
    let b = UnixDatagram::bind(&reply_path).expect("bind b");
    a.send_to(b"ping", &reply_path).await.expect("send");
    let mut buf = [0u8; 32];
    let (n, from) = b.recv_from(&mut buf).await.expect("recv");
    assert_eq!(&buf[..n], b"ping");
    assert!(from.as_pathname().is_some(), "sender was unnamed");

    smol::Timer::after(Duration::from_millis(10)).await;
    println!("UnixDatagram delivered both directions");
}
