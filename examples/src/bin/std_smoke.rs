//! Exercises the std paths library.patch touches: stat layout, fcntl-based
//! non-blocking, socket option translation, MSG_NOSIGNAL, SOCK_CLOEXEC, thread
//! naming, and the random source. Problems there usually show up as a panic
//! or a hang, so it is enough to run this and check that it exits 0.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

fn main() {
    // struct stat layout — libc.patch redefines this for aarch64.
    let cwd = std::env::current_dir().expect("current_dir");
    assert!(fs::metadata(&cwd).expect("stat cwd").is_dir(), "cwd is a dir");
    // Note current_exe returns the APE loader, not our program (it can't see
    // through the bootstrap; use ape::program_executable_name() for that).
    // Either way it's a real file, which is enough for the stat check here.
    let exe = std::env::current_exe().expect("current_exe");
    let md = fs::metadata(&exe).expect("stat exe");
    assert!(md.is_file(), "exe is a regular file");
    assert!(!md.is_dir(), "exe is not a dir");
    assert!(md.len() > 0, "exe is not empty");
    let entries = fs::read_dir(&cwd).expect("read_dir").count();
    println!("fs   ok: exe {} bytes, cwd has {} entries", md.len(), entries);

    // pthread_setname_np returns ENOSYS off Linux and must not be fatal.
    let name = thread::Builder::new()
        .name("cosmo-worker".into())
        .spawn(|| thread::current().name().map(str::to_owned))
        .expect("spawn")
        .join()
        .expect("join");
    assert_eq!(name.as_deref(), Some("cosmo-worker"));
    println!("thd  ok: thread named {:?}", name.unwrap());

    // SOCK_CLOEXEC, setsockopt translation, MSG_NOSIGNAL, fcntl non-blocking.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let server = thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 5];
        sock.read_exact(&mut buf).expect("server read");
        sock.write_all(b"pong!").expect("server write");
    });

    let mut client = TcpStream::connect(addr).expect("connect");
    client.set_nodelay(true).expect("set_nodelay"); // TCP_NODELAY
    assert!(client.nodelay().expect("nodelay"), "nodelay stuck"); // getsockopt
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set_read_timeout"); // SO_RCVTIMEO
    client.set_nonblocking(true).expect("nonblocking"); // fcntl F_SETFL
    client.set_nonblocking(false).expect("blocking again");
    client.write_all(b"ping!").expect("client write"); // send(MSG_NOSIGNAL)
    let mut buf = [0u8; 5];
    client.read_exact(&mut buf).expect("client read");
    server.join().expect("server join");
    assert_eq!(&buf, b"pong!");
    println!("net  ok: {} round-tripped", String::from_utf8_lossy(&buf));

    // Random source: getrandom, or /dev/urandom where that's unavailable.
    let seeds: Vec<_> = (0..4).map(|_| std::hash::RandomState::new()).collect();
    println!("rand ok: {} RandomState built", seeds.len());

    println!("\nstd smoke test passed");
}
