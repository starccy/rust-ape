//! test getrandom and async

use rand::Rng;
use smol::io::{AsyncReadExt, AsyncWriteExt};
use smol::net::{TcpListener, TcpStream};

fn main() {
    let n: u32 = rand::thread_rng().gen_range(1000..9999);
    println!("rand ok: {n}");

    smol::block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let server = smol::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 5];
            sock.read_exact(&mut buf).await.expect("server read");
            sock.write_all(b"pong!").await.expect("server write");
        });

        let mut client = TcpStream::connect(addr).await.expect("connect");
        client.write_all(b"ping!").await.expect("client write");
        let mut buf = [0u8; 5];
        client.read_exact(&mut buf).await.expect("client read");
        server.await;

        assert_eq!(&buf, b"pong!");
        println!("smol ok: {} round-tripped", String::from_utf8_lossy(&buf));
    });

    println!("\ndependency smoke test passed");
}
