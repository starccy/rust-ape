//! SCM_RIGHTS: descriptors sent over a unix stream socket to another
//! process. Linux and macOS have it in the kernel; on Windows the fork
//! duplicates the handles into the peer and carries them in the stream.
//!
//! The parent listens on a unix socket and spawns itself as the child.
//! The child opens a file, connects a tcp socket back to the parent, and
//! sends both descriptors in one sendmsg. The parent reads the file
//! through the received descriptor and exchanges a ping over the socket.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn send_fds(sock: &UnixStream, fds: &[i32], data: &[u8]) -> isize {
    let space = unsafe { libc::CMSG_SPACE((fds.len() * 4) as u32) } as usize;
    let mut cbuf = vec![0u8; space];
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: data.len(),
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf.as_mut_ptr() as *mut _;
    msg.msg_controllen = space as _;
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN((fds.len() * 4) as u32) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(c) as *mut i32, fds.len());
        libc::sendmsg(sock.as_raw_fd(), &msg, 0)
    }
}

fn recv_fds(sock: &UnixStream, want: usize, data: &mut [u8]) -> (isize, Vec<i32>) {
    let space = unsafe { libc::CMSG_SPACE((want * 4) as u32) } as usize;
    let mut cbuf = vec![0u8; space];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr() as *mut _,
        iov_len: data.len(),
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf.as_mut_ptr() as *mut _;
    msg.msg_controllen = space as _;
    let n = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
    let mut fds = Vec::new();
    if n >= 0 {
        let mut c = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        while !c.is_null() {
            let (level, ty, len) =
                unsafe { ((*c).cmsg_level, (*c).cmsg_type, (*c).cmsg_len as usize) };
            if level == libc::SOL_SOCKET && ty == libc::SCM_RIGHTS {
                let count = (len - unsafe { libc::CMSG_LEN(0) } as usize) / 4;
                let p = unsafe { libc::CMSG_DATA(c) } as *const i32;
                for i in 0..count {
                    fds.push(unsafe { *p.add(i) });
                }
            }
            c = unsafe { libc::CMSG_NXTHDR(&msg, c) };
        }
    }
    (n, fds)
}

fn child(sock_path: &str, tcp_addr: &str, file_path: &str) {
    let sock = UnixStream::connect(sock_path).expect("connect to parent");
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(file_path)
        .unwrap();
    f.write_all(b"hello from child").unwrap();
    let tcp = TcpStream::connect(tcp_addr).expect("tcp to parent");
    let n = send_fds(&sock, &[f.as_raw_fd(), tcp.as_raw_fd()], b"go");
    assert_eq!(n, 2, "sendmsg errno {}", errno());
    drop(f);
    drop(tcp);
    let mut ack = [0u8; 1];
    let mut s = &sock;
    s.read_exact(&mut ack).unwrap();
    assert_eq!(&ack, b"k");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 5 && args[1] == "--child" {
        child(&args[2], &args[3], &args[4]);
        return;
    }

    let dir = std::env::temp_dir().join(format!("fd_passing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock_path = dir.join("sock");
    let file_path = dir.join("passed");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let tcp_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp_addr = tcp_listener.local_addr().unwrap().to_string();

    // the same message through a socketpair inside one process first
    {
        let (a, b) = UnixStream::pair().unwrap();
        let (mut r, mut w) = {
            let mut fds = [0; 2];
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
            unsafe {
                (
                    std::fs::File::from_raw_fd(fds[0]),
                    std::fs::File::from_raw_fd(fds[1]),
                )
            }
        };
        assert_eq!(
            send_fds(&a, &[r.as_raw_fd()], b"x"),
            1,
            "in-process sendmsg errno {}",
            errno()
        );
        let mut byte = [0u8; 1];
        let (n, fds) = recv_fds(&b, 1, &mut byte);
        assert_eq!(
            (n, fds.len(), byte[0]),
            (1, 1, b'x'),
            "in-process recvmsg errno {}",
            errno()
        );
        let got = unsafe { libc::fcntl(fds[0], libc::F_GETFD) };
        assert!(got & libc::FD_CLOEXEC != 0, "MSG_CMSG_CLOEXEC not applied");
        w.write_all(b"q").unwrap();
        let mut copy = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut q = [0u8; 1];
        copy.read_exact(&mut q).unwrap();
        assert_eq!(&q, b"q");
        let mut _keep = &mut r;
        println!("in-process: pipe end passed, readable, close-on-exec");
    }

    let exe = std::env::current_exe().unwrap();
    let mut proc = std::process::Command::new(exe)
        .args([
            "--child",
            sock_path.to_str().unwrap(),
            &tcp_addr,
            file_path.to_str().unwrap(),
        ])
        .spawn()
        .expect("spawn child");
    let (mut conn, _) = listener.accept().unwrap();
    let (mut tcp_peer, _) = tcp_listener.accept().unwrap();

    let mut two = [0u8; 2];
    let (n, fds) = recv_fds(&conn, 2, &mut two);
    assert_eq!(n, 2, "recvmsg from child errno {}", errno());
    assert_eq!(&two, b"go");
    assert_eq!(fds.len(), 2, "expected two descriptors, got {fds:?}");

    let mut file = unsafe { std::fs::File::from_raw_fd(fds[0]) };
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "", "the child's write offset carries over");
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(0)).unwrap();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "hello from child");

    let mut tcp = unsafe { TcpStream::from_raw_fd(fds[1]) };
    tcp.write_all(b"ping").unwrap();
    let mut ping = [0u8; 4];
    tcp_peer.read_exact(&mut ping).unwrap();
    assert_eq!(&ping, b"ping");
    tcp_peer.write_all(b"pong").unwrap();
    tcp.read_exact(&mut ping).unwrap();
    assert_eq!(&ping, b"pong");

    conn.write_all(b"k").unwrap();
    let st = proc.wait().unwrap();
    assert!(st.success(), "child status {st}");
    drop(listener);
    std::fs::remove_dir_all(&dir).unwrap();
    println!(
        "cross-process: file offset {} bytes in, tcp ping/pong through the passed socket",
        text.len()
    );
    println!("ok");
}
