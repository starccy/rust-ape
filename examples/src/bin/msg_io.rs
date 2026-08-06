//! sendmsg/recvmsg through the shim. Three Linux-coded values pass through
//! these calls: the MSG_* flags argument, the address family inside
//! msg_name, and cmsg_level inside the control payload (SOL_SOCKET is 1 on
//! Linux but 0xffff on the BSDs). The msghdr/cmsghdr structs themselves are
//! byte-compatible with musl, so nothing here repacks.
//!
//! Two scenarios: a UDP round-trip via raw sendmsg/recvmsg, where the reply
//! address family must come back as musl's AF_INET, and fd passing over a
//! unix socketpair with SCM_RIGHTS, which exercises cmsg_level translation
//! in both directions.

use std::io::{Read, Seek, Write};
use std::mem;
use std::os::fd::AsRawFd;

fn udp_msg_roundtrip() {
    let a = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let b = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let b_port = b.local_addr().unwrap().port();

    unsafe {
        let mut dst: libc::sockaddr_in = mem::zeroed();
        dst.sin_family = libc::AF_INET as libc::sa_family_t;
        dst.sin_port = b_port.to_be();
        dst.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();

        let payload = b"via sendmsg";
        let mut iov = libc::iovec {
            iov_base: payload.as_ptr() as *mut _,
            iov_len: payload.len(),
        };
        let mut msg: libc::msghdr = mem::zeroed();
        msg.msg_name = &mut dst as *mut _ as *mut _;
        msg.msg_namelen = mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        let sent = libc::sendmsg(a.as_raw_fd(), &msg, 0);
        assert_eq!(sent, payload.len() as isize, "{}", std::io::Error::last_os_error());

        let mut buf = [0u8; 32];
        let mut from: libc::sockaddr_in = mem::zeroed();
        let mut riov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut _,
            iov_len: buf.len(),
        };
        let mut rmsg: libc::msghdr = mem::zeroed();
        rmsg.msg_name = &mut from as *mut _ as *mut _;
        rmsg.msg_namelen = mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        rmsg.msg_iov = &mut riov;
        rmsg.msg_iovlen = 1;
        let got = libc::recvmsg(b.as_raw_fd(), &mut rmsg, 0);
        assert_eq!(got, payload.len() as isize, "{}", std::io::Error::last_os_error());
        assert_eq!(&buf[..payload.len()], payload);
        assert_eq!(from.sin_family as i32, libc::AF_INET,
            "recvmsg msg_name family not musl-coded");
        assert_eq!(u16::from_be(from.sin_port), a.local_addr().unwrap().port());
        assert_eq!(rmsg.msg_flags & libc::MSG_TRUNC, 0);
    }
    println!("udp sendmsg/recvmsg round-trip ok (families musl-coded)");
}

fn scm_rights_fd_passing() {
    // A real file whose fd travels across a unix socketpair.
    let mut tmp = tempfile::tempfile().expect("tempfile");
    tmp.write_all(b"smuggled").unwrap();
    tmp.rewind().unwrap();

    unsafe {
        let mut sv = [0i32; 2];
        assert_eq!(libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()), 0);

        // sender: one data byte + SCM_RIGHTS cmsg carrying the file's fd
        let data = [b'!'];
        let mut iov = libc::iovec {
            iov_base: data.as_ptr() as *mut _,
            iov_len: 1,
        };
        let mut cbuf = [0u8; 64]; // CMSG_SPACE(4) fits with room to spare
        let mut msg: libc::msghdr = mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf.as_mut_ptr() as *mut _;
        msg.msg_controllen = libc::CMSG_SPACE(4) as _;
        let cm = libc::CMSG_FIRSTHDR(&msg);
        (*cm).cmsg_level = libc::SOL_SOCKET;
        (*cm).cmsg_type = libc::SCM_RIGHTS;
        (*cm).cmsg_len = libc::CMSG_LEN(4) as _;
        std::ptr::copy_nonoverlapping(
            &tmp.as_raw_fd() as *const i32 as *const u8,
            libc::CMSG_DATA(cm),
            4,
        );
        let sent = libc::sendmsg(sv[0], &msg, 0);
        assert_eq!(sent, 1, "sendmsg(SCM_RIGHTS): {}", std::io::Error::last_os_error());

        // receiver
        let mut rdata = [0u8; 1];
        let mut riov = libc::iovec {
            iov_base: rdata.as_mut_ptr() as *mut _,
            iov_len: 1,
        };
        let mut rcbuf = [0u8; 64];
        let mut rmsg: libc::msghdr = mem::zeroed();
        rmsg.msg_iov = &mut riov;
        rmsg.msg_iovlen = 1;
        rmsg.msg_control = rcbuf.as_mut_ptr() as *mut _;
        rmsg.msg_controllen = rcbuf.len() as _;
        let got = libc::recvmsg(sv[1], &mut rmsg, 0);
        assert_eq!(got, 1, "recvmsg(SCM_RIGHTS): {}", std::io::Error::last_os_error());
        assert_eq!(rdata[0], b'!');

        let rcm = libc::CMSG_FIRSTHDR(&rmsg);
        assert!(!rcm.is_null(), "no cmsg came back");
        assert_eq!((*rcm).cmsg_level, libc::SOL_SOCKET, "cmsg_level not musl-coded on receive");
        assert_eq!((*rcm).cmsg_type, libc::SCM_RIGHTS);
        let mut passed_fd = 0i32;
        std::ptr::copy_nonoverlapping(libc::CMSG_DATA(rcm), &mut passed_fd as *mut i32 as *mut u8, 4);
        assert!(passed_fd >= 0 && passed_fd != tmp.as_raw_fd(), "no new fd materialized");

        // the smuggled fd must actually work
        let mut carried = std::fs::File::from(std::os::fd::OwnedFd::from_raw_fd(passed_fd));
        let mut s = String::new();
        carried.read_to_string(&mut s).unwrap();
        assert_eq!(s, "smuggled");

        libc::close(sv[0]);
        libc::close(sv[1]);
    }
    println!("SCM_RIGHTS fd passing ok (cmsg_level translated both ways)");
}

use std::os::fd::FromRawFd;

fn main() {
    udp_msg_roundtrip();

    // NT has no fd passing over AF_UNIX; the shim faithfully forwards and the
    // host says no. Only assert the full path where the host supports it —
    // the compile-time cfg is always "linux" here, so detect the host at
    // runtime instead.
    let is_nt = std::fs::metadata("C:\\Windows").is_ok();
    if is_nt {
        println!("SCM_RIGHTS skipped: NT has no unix-socket fd passing");
    } else {
        scm_rights_fd_passing();
    }

    println!("\nmsg io ok");
}
