//! sendmmsg/recvmmsg, fallocate/posix_fallocate, close_range and
//! memfd_create through the libc crate. The fork carries all of them on
//! every host; Linux uses the kernel, the rest are built on sendmsg/recvmsg,
//! the host's preallocation call, a walk over the descriptor table, and an
//! unlinked temp file.
//!
//! The musl flavour of the libc crate declares fallocate and the constants
//! but not sendmmsg, recvmmsg or close_range, so those are declared here
//! the way a crate that wants them would.

use std::io::{Read, Seek, SeekFrom, Write};
use std::net::UdpSocket;
use std::os::fd::{AsRawFd, FromRawFd};

unsafe extern "C" {
    fn sendmmsg(
        fd: libc::c_int,
        vec: *mut libc::mmsghdr,
        vlen: libc::c_uint,
        flags: libc::c_int,
    ) -> libc::c_int;
    fn recvmmsg(
        fd: libc::c_int,
        vec: *mut libc::mmsghdr,
        vlen: libc::c_uint,
        flags: libc::c_int,
        timeout: *mut libc::timespec,
    ) -> libc::c_int;
    fn close_range(first: libc::c_uint, last: libc::c_uint, flags: libc::c_uint) -> libc::c_int;
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn fallocate_checks() {
    let dir = std::env::temp_dir().join(format!("mmsg_fallocate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("prealloc");
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    let fd = f.as_raw_fd();

    let r = unsafe { libc::fallocate(fd, 0, 0, 65536) };
    assert_eq!(r, 0, "fallocate errno {}", errno());
    assert_eq!(f.metadata().unwrap().len(), 65536);

    let r = unsafe { libc::fallocate(fd, libc::FALLOC_FL_KEEP_SIZE, 0, 262144) };
    assert_eq!(r, 0, "fallocate KEEP_SIZE errno {}", errno());
    assert_eq!(
        f.metadata().unwrap().len(),
        65536,
        "KEEP_SIZE grew the file"
    );

    let r = unsafe { libc::posix_fallocate(fd, 100000, 1000) };
    assert_eq!(r, 0, "posix_fallocate returned {r}");
    assert_eq!(f.metadata().unwrap().len(), 101000);

    f.seek(SeekFrom::Start(70000)).unwrap();
    let mut z = [1u8; 16];
    f.read_exact(&mut z).unwrap();
    assert!(z.iter().all(|&b| b == 0), "extended range not zero");

    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(b"still writable").unwrap();

    let r = unsafe { libc::fallocate(fd, 0, -1, 10) };
    assert!(
        r == -1 && errno() == libc::EINVAL,
        "offset -1 gave {r}/{}",
        errno()
    );
    let r = unsafe { libc::posix_fallocate(-1, 0, 10) };
    assert_eq!(r, libc::EBADF);

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let r = unsafe { libc::fallocate(pipe[1], 0, 0, 10) };
    let e = errno();
    assert!(
        r == -1 && (e == libc::ESPIPE || e == libc::ENODEV),
        "pipe gave {r}/{e}"
    );
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }

    drop(f);
    std::fs::remove_dir_all(&dir).unwrap();
    println!("fallocate: 64K, KEEP_SIZE, posix_fallocate, zero fill, EINVAL, EBADF, pipe ok");
}

fn mmsg_checks() {
    let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
    let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
    let dst: libc::sockaddr_in = {
        let a = match rx.local_addr().unwrap() {
            std::net::SocketAddr::V4(a) => a,
            _ => unreachable!(),
        };
        let mut s: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        s.sin_family = libc::AF_INET as libc::sa_family_t;
        s.sin_port = a.port().to_be();
        s.sin_addr.s_addr = u32::from(*a.ip()).to_be();
        s
    };

    let payloads: [&[u8]; 3] = [b"one", b"twoo", b"three"];
    let mut iov: Vec<libc::iovec> = payloads
        .iter()
        .map(|p| libc::iovec {
            iov_base: p.as_ptr() as *mut _,
            iov_len: p.len(),
        })
        .collect();
    let mut out: Vec<libc::mmsghdr> = (0..3)
        .map(|i| {
            let mut m: libc::mmsghdr = unsafe { std::mem::zeroed() };
            m.msg_hdr.msg_iov = &mut iov[i];
            m.msg_hdr.msg_iovlen = 1;
            m.msg_hdr.msg_name = &dst as *const _ as *mut _;
            m.msg_hdr.msg_namelen = std::mem::size_of::<libc::sockaddr_in>() as u32;
            m
        })
        .collect();
    let n = unsafe { sendmmsg(tx.as_raw_fd(), out.as_mut_ptr(), 3, 0) };
    assert_eq!(n, 3, "sendmmsg errno {}", errno());
    assert_eq!([out[0].msg_len, out[1].msg_len, out[2].msg_len], [3, 4, 5]);

    let mut bufs = [[0u8; 16]; 4];
    let mut riov: Vec<libc::iovec> = bufs
        .iter_mut()
        .map(|b| libc::iovec {
            iov_base: b.as_mut_ptr() as *mut _,
            iov_len: 16,
        })
        .collect();
    let mut inn: Vec<libc::mmsghdr> = (0..4)
        .map(|i| {
            let mut m: libc::mmsghdr = unsafe { std::mem::zeroed() };
            m.msg_hdr.msg_iov = &mut riov[i];
            m.msg_hdr.msg_iovlen = 1;
            m
        })
        .collect();
    // Linux checks the timeout only after a datagram arrives, so asking for
    // four with three queued blocks for good unless MSG_WAITFORONE is set
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 300_000_000,
    };
    let n = unsafe {
        recvmmsg(
            rx.as_raw_fd(),
            inn.as_mut_ptr(),
            4,
            libc::MSG_WAITFORONE,
            &mut t,
        )
    };
    assert_eq!(n, 3, "recvmmsg errno {}", errno());
    assert_eq!([inn[0].msg_len, inn[1].msg_len, inn[2].msg_len], [3, 4, 5]);
    assert_eq!(&bufs[2][..5], b"three");

    let n = unsafe {
        recvmmsg(
            rx.as_raw_fd(),
            inn.as_mut_ptr(),
            4,
            libc::MSG_DONTWAIT,
            std::ptr::null_mut(),
        )
    };
    assert!(
        n == -1 && errno() == libc::EAGAIN,
        "empty recvmmsg gave {n}/{}",
        errno()
    );

    let n = unsafe { sendmmsg(tx.as_raw_fd(), out.as_mut_ptr(), 2, 0) };
    assert_eq!(n, 2);
    std::thread::sleep(std::time::Duration::from_millis(50));
    let n = unsafe {
        recvmmsg(
            rx.as_raw_fd(),
            inn.as_mut_ptr(),
            4,
            libc::MSG_WAITFORONE,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(n, 2, "MSG_WAITFORONE gave {n}/{}", errno());
    println!("sendmmsg/recvmmsg: 3 out, 3 in, EAGAIN when empty, WAITFORONE 2 ok");
}

fn close_range_checks() {
    let open = || unsafe { libc::open(b"/dev/null\0".as_ptr() as *const _, libc::O_RDONLY) };
    let a = open();
    let b = open();
    let c = open();
    assert!(a >= 0 && b == a + 1 && c == b + 1, "fds {a} {b} {c}");
    let r = unsafe { close_range(a as u32, c as u32, libc::CLOSE_RANGE_CLOEXEC) };
    assert_eq!(r, 0, "close_range(CLOEXEC) errno {}", errno());
    assert!(unsafe { libc::fcntl(b, libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
    let r = unsafe { close_range(a as u32, c as u32, 0) };
    assert_eq!(r, 0, "close_range errno {}", errno());
    assert_eq!(unsafe { libc::fcntl(a, libc::F_GETFD) }, -1);
    assert_eq!(unsafe { libc::fcntl(c, libc::F_GETFD) }, -1);
    assert_eq!(unsafe { close_range(4000, u32::MAX, 0) }, 0);
    let r = unsafe { close_range(5, 3, 0) };
    assert!(r == -1 && errno() == libc::EINVAL);
    println!("close_range: CLOEXEC, close, past the table, EINVAL ok");
}

fn memfd_checks() {
    let fd = unsafe { libc::memfd_create(b"example\0".as_ptr() as *const _, libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create errno {}", errno());
    assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
    let mut f = unsafe { std::fs::File::from_raw_fd(fd) };
    f.write_all(b"in memory").unwrap();
    f.set_len(4096).unwrap();
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    assert!(map != libc::MAP_FAILED, "mmap errno {}", errno());
    let bytes = unsafe { std::slice::from_raw_parts_mut(map as *mut u8, 9) };
    assert_eq!(&bytes[..9], b"in memory");
    bytes[..2].copy_from_slice(b"IN");
    unsafe { libc::munmap(map, 4096) };
    f.seek(SeekFrom::Start(0)).unwrap();
    let mut back = [0u8; 9];
    f.read_exact(&mut back).unwrap();
    assert_eq!(&back, b"IN memory");
    assert_eq!(f.metadata().unwrap().len(), 4096);
    println!("memfd_create: close-on-exec, write, set_len, shared mapping writes through");
}

fn main() {
    fallocate_checks();
    mmsg_checks();
    close_range_checks();
    memfd_checks();
    println!("ok");
}
