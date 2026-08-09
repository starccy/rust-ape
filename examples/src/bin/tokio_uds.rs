//! Unix domain sockets on the runtime.
//!
//! Datagrams don't work on Windows: `socket(AF_UNIX, SOCK_DGRAM)` is answered with
//! WSAEAFNOSUPPORT, Windows' AF_UNIX being stream-only, so that part only runs
//! off Windows.
//!
//! Paths go through the shim, which runs sun_path through cosmo's own
//! translation before Winsock sees it.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixDatagram, UnixListener, UnixStream};

const CLIENTS: usize = 8;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        let dir = std::env::temp_dir().join(format!("rust-ape-uds-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.expect("create_dir_all");

        streams(&dir).await;
        pairs().await;
        if ape::is_windows() {
            println!("skipping datagrams: NT's AF_UNIX has no SOCK_DGRAM");
        } else {
            datagrams(&dir).await;
        }

        tokio::fs::remove_dir_all(&dir).await.expect("remove_dir_all");
    });

    println!("\ntokio uds ok");
}

async fn streams(dir: &std::path::Path) {
    let path = dir.join("stream.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    println!("listening on {}", path.display());

    let server = tokio::spawn(async move {
        for _ in 0..CLIENTS {
            let (mut sock, _) = listener.accept().await.expect("accept");
            tokio::spawn(async move {
                // Echo whatever arrives until the peer hangs up. A single read
                // would be a bet that the message never gets split.
                let mut buf = [0u8; 64];
                loop {
                    let n = sock.read(&mut buf).await.expect("server read");
                    if n == 0 {
                        return;
                    }
                    sock.write_all(&buf[..n]).await.expect("server write");
                }
            });
        }
    });

    let clients: Vec<_> = (0..CLIENTS)
        .map(|id| {
            let path = path.clone();
            tokio::spawn(async move {
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
        c.await.expect("client");
    }
    server.await.expect("server");
    tokio::fs::remove_file(&path).await.expect("remove socket");
    println!("{CLIENTS} clients echoed over a listening socket");
}

/// socketpair() rather than a path, which is how most process-to-child plumbing
/// actually uses AF_UNIX.
async fn pairs() {
    let (mut a, mut b) = UnixStream::pair().expect("pair");
    let echo = tokio::spawn(async move {
        let mut buf = [0u8; 6];
        b.read_exact(&mut buf).await.expect("read");
        b.write_all(&buf).await.expect("write");
    });

    a.write_all(b"paired").await.expect("write");
    let mut back = [0u8; 6];
    a.read_exact(&mut back).await.expect("read");
    assert_eq!(&back, b"paired");
    echo.await.expect("echo");
    println!("UnixStream::pair round-tripped");
}

async fn datagrams(dir: &std::path::Path) {
    let server_path = dir.join("dgram.sock");
    let server = UnixDatagram::bind(&server_path).expect("bind server");
    let client = UnixDatagram::unbound().expect("unbound client");

    let echo = tokio::spawn(async move {
        let mut buf = [0u8; 64];
        let (n, _) = server.recv_from(&mut buf).await.expect("recv_from");
        // The client is unbound, so there's nowhere to reply; the receive is
        // the whole assertion here.
        assert_eq!(&buf[..n], b"datagram");
        n
    });

    client.send_to(b"datagram", &server_path).await.expect("send_to");
    let n = tokio::time::timeout(Duration::from_secs(10), echo)
        .await
        .expect("datagram never arrived")
        .expect("echo task");
    assert_eq!(n, 8);

    tokio::fs::remove_file(&server_path).await.expect("remove socket");
    println!("UnixDatagram delivered {n} bytes");
}
