//! tokio-util's codec layer and CancellationToken, the two pieces most tokio
//! programs end up pulling in on top of the runtime itself.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{Framed, LengthDelimitedCodec, LinesCodec};
use tokio_util::sync::CancellationToken;

const CLIENTS: usize = 16;
const LINES: usize = 32;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        lines().await;
        length_delimited().await;
        cancellation().await;
    });

    println!("\ntokio codec ok");
}

async fn lines() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let server = tokio::spawn(async move {
        for _ in 0..CLIENTS {
            let (sock, _) = listener.accept().await.expect("accept");
            tokio::spawn(async move {
                let mut framed = Framed::new(sock, LinesCodec::new());
                while let Some(line) = framed.next().await {
                    let line = line.expect("decode");
                    framed.send(line.to_uppercase()).await.expect("encode");
                }
            });
        }
    });

    let clients: Vec<_> = (0..CLIENTS)
        .map(|id| {
            tokio::spawn(async move {
                let sock = TcpStream::connect(addr).await.expect("connect");
                let mut framed = Framed::new(sock, LinesCodec::new());
                for i in 0..LINES {
                    let msg = format!("client-{id}-line-{i}");
                    framed.send(msg.clone()).await.expect("send");
                    let back = framed.next().await.expect("stream ended").expect("decode");
                    assert_eq!(back, msg.to_uppercase(), "client {id} got {back:?}");
                }
            })
        })
        .collect();

    for c in clients {
        c.await.expect("client");
    }
    server.await.expect("server");
    println!("LinesCodec: {} framed roundtrips over TCP", CLIENTS * LINES);
}

/// Length-delimited frames of 64KB, which is well past one read's worth and
/// forces the decoder to buffer across several wakeups.
async fn length_delimited() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.expect("accept");
        let mut framed = Framed::new(sock, LengthDelimitedCodec::new());
        let mut frames = 0;
        while let Some(frame) = framed.next().await {
            let frame = frame.expect("decode");
            framed.send(frame.freeze()).await.expect("encode");
            frames += 1;
        }
        frames
    });

    let sock = TcpStream::connect(addr).await.expect("connect");
    let mut framed = Framed::new(sock, LengthDelimitedCodec::new());
    for i in 0u8..8 {
        let payload = vec![i; 64 * 1024];
        framed.send(payload.clone().into()).await.expect("send");
        let back = framed.next().await.expect("stream ended").expect("decode");
        assert_eq!(back.len(), payload.len(), "frame came back {} bytes", back.len());
        assert!(back.iter().all(|&b| b == i), "frame {i} came back corrupted");
    }
    drop(framed);

    assert_eq!(server.await.expect("server"), 8);
    println!("LengthDelimitedCodec: 8 frames of 64KB round-tripped intact");
}

async fn cancellation() {
    let token = CancellationToken::new();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let token = token.child_token();
        tasks.push(tokio::spawn(async move {
            tokio::select! {
                _ = token.cancelled() => "cancelled",
                _ = tokio::time::sleep(Duration::from_secs(60)) => "slept",
            }
        }));
    }

    tokio::time::sleep(Duration::from_millis(30)).await;
    token.cancel();
    for t in tasks {
        assert_eq!(t.await.expect("task"), "cancelled", "a child token was missed");
    }

    // Already-cancelled tokens resolve immediately rather than parking forever.
    tokio::time::timeout(Duration::from_secs(5), token.cancelled())
        .await
        .expect("an already-cancelled token parked");
    println!("CancellationToken: 8 child tokens cancelled from the parent");
}
