//! Async TCP echo with smol, several clients at once.

use smol::io::{AsyncReadExt, AsyncWriteExt};
use smol::net::{TcpListener, TcpStream};
use smol::stream::StreamExt;

const CLIENTS: usize = 8;

fn main() {
    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        println!("listening on {addr}");

        // Accept loop: one task per connection, so the clients below really do
        // overlap rather than queueing up behind each other.
        let server = smol::spawn(async move {
            let mut incoming = listener.incoming().take(CLIENTS);
            let mut tasks = Vec::new();
            while let Some(sock) = incoming.next().await {
                let sock = sock.expect("accept");
                tasks.push(smol::spawn(async move {
                    let mut sock = sock;
                    let mut buf = vec![0u8; 256];
                    let n = sock.read(&mut buf).await.expect("server read");
                    sock.write_all(&buf[..n]).await.expect("server write");
                    n
                }));
            }
            let mut echoed = 0usize;
            for t in tasks {
                echoed += t.await;
            }
            echoed
        });

        // Fire all the clients off before awaiting any of them.
        let clients: Vec<_> = (0..CLIENTS)
            .map(|id| {
                smol::spawn(async move {
                    let msg = format!("client-{id}");
                    let mut sock = TcpStream::connect(addr).await.expect("connect");
                    sock.write_all(msg.as_bytes()).await.expect("client write");

                    let mut back = vec![0u8; msg.len()];
                    sock.read_exact(&mut back).await.expect("client read");
                    assert_eq!(back, msg.as_bytes(), "echo differs from what we sent");
                    msg
                })
            })
            .collect();

        let mut sent = 0usize;
        for c in clients {
            let msg = c.await;
            sent += msg.len();
            println!("echoed {msg:?}");
        }

        let echoed = server.await;
        assert_eq!(echoed, sent, "server echoed {echoed} bytes, clients sent {sent}");

        println!("\nsmol tcp echo ok: {CLIENTS} concurrent clients, {sent} bytes");
    });
}
