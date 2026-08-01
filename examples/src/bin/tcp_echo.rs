//! Blocking TCP echo server and client on one loopback socket pair.
//!
//! Touches the socket options std has to translate per host: SOCK_CLOEXEC on
//! creation, TCP_NODELAY and SO_RCVTIMEO through set/getsockopt, O_NONBLOCK via
//! fcntl, and MSG_NOSIGNAL on send.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

const MESSAGES: [&str; 4] = ["hello", "cosmopolitan", "actually portable", "bye"];

fn main() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    println!("listening on {addr}");

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

    // Half-close so the server's read returns 0 and its loop ends.
    client.shutdown(std::net::Shutdown::Write).expect("shutdown");
    let echoed = server.join().expect("server thread");
    assert_eq!(echoed, sent, "server echoed {echoed} bytes, we sent {sent}");

    println!("\ntcp echo ok: {sent} bytes round-tripped");
}
