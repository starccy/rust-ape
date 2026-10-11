//! Capturing traffic through an `AF_PACKET` socket, the way a Linux sniffer
//! does: open, bind to an interface, turn on promiscuous mode, read frames.
//!
//! Needs privileges everywhere. Without them (or on a host with no packet
//! sockets at all) the open fails and the run is reported as skipped, so
//! the suite can carry it. Windows serves it through cosmo's
//! libc/sock/packet-nt.c, which builds the capture out of a raw IP socket
//! in `SIO_RCVALL` mode and synthesizes the Ethernet header a caller
//! expects; the cooked `SOCK_DGRAM` form and plain `read()` are checked
//! too, since a sniffer may use either.
//!
//! Traffic to look at is made on the spot: a few UDP datagrams and one
//! `ping` to the gateway. `PACKET_IFACE` picks the interface, `PACKET_PEER`
//! the address to ping; both are guessed otherwise.

use std::ffi::CStr;
use std::io;
use std::mem;
use std::net::{Ipv4Addr, UdpSocket};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rust_ape_examples::Report;

fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn os_err(e: i32) -> String {
    format!("errno {e} ({})", io::Error::from_raw_os_error(e))
}

/// The interfaces if_nameindex lists, loopback left out.
fn interfaces() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    unsafe {
        let list = libc::if_nameindex();
        if list.is_null() {
            return out;
        }
        let mut p = list;
        while (*p).if_index != 0 {
            let name = CStr::from_ptr((*p).if_name).to_string_lossy().into_owned();
            if !name.to_ascii_lowercase().starts_with("lo") {
                out.push(((*p).if_index, name));
            }
            p = p.add(1);
        }
        libc::if_freenameindex(list);
    }
    out
}

fn bind_packet(fd: i32, ifindex: u32) -> Result<(), i32> {
    let mut sll: libc::sockaddr_ll = unsafe { mem::zeroed() };
    sll.sll_family = libc::AF_PACKET as u16;
    sll.sll_protocol = (libc::ETH_P_ALL as u16).to_be();
    sll.sll_ifindex = ifindex as i32;
    let rc = unsafe {
        libc::bind(
            fd,
            &sll as *const libc::sockaddr_ll as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
        )
    };
    if rc == 0 { Ok(()) } else { Err(errno()) }
}

/// The address the default route leaves from, and a guess at its gateway.
fn local_and_gateway() -> Option<(Ipv4Addr, Ipv4Addr)> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:53").ok()?;
    let local = match s.local_addr().ok()? {
        std::net::SocketAddr::V4(a) => *a.ip(),
        _ => return None,
    };
    let o = local.octets();
    Some((local, Ipv4Addr::new(o[0], o[1], o[2], 1)))
}

fn make_traffic(peer: Ipv4Addr) {
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        for i in 0..4 {
            let _ = s.send_to(&[0x55; 32], (peer, 9 + i));
        }
    }
    let windows = std::env::var("OS").as_deref() == Ok("Windows_NT");
    let args: Vec<String> = if windows {
        vec![
            "-n".into(),
            "1".into(),
            "-w".into(),
            "1000".into(),
            peer.to_string(),
        ]
    } else {
        vec![
            "-c".into(),
            "1".into(),
            "-W".into(),
            "1".into(),
            peer.to_string(),
        ]
    };
    let _ = Command::new("ping")
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

struct Seen {
    frames: usize,
    ipv4: usize,
    ours: usize,
    icmp: usize,
    other_proto: usize,
    bad_sll: Vec<String>,
}

fn capture(fd: i32, ifindex: u32, local: Ipv4Addr, peer: Ipv4Addr, budget: Duration) -> Seen {
    let mut seen = Seen {
        frames: 0,
        ipv4: 0,
        ours: 0,
        icmp: 0,
        other_proto: 0,
        bad_sll: Vec::new(),
    };
    let mut buf = [0u8; 65536];
    let start = Instant::now();
    loop {
        let left = budget.checked_sub(start.elapsed()).unwrap_or_default();
        if left.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut pfd, 1, left.as_millis() as i32) };
        if n <= 0 {
            break;
        }
        let mut sll: libc::sockaddr_ll = unsafe { mem::zeroed() };
        let mut alen = mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
        let r = unsafe {
            libc::recvfrom(
                fd,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
                &mut sll as *mut libc::sockaddr_ll as *mut libc::sockaddr,
                &mut alen,
            )
        };
        if r < 0 {
            let e = errno();
            if e == libc::EAGAIN || e == libc::EINTR {
                continue;
            }
            println!("recvfrom failed: {}", os_err(e));
            break;
        }
        let frame = &buf[..r as usize];
        if frame.is_empty() {
            continue;
        }
        seen.frames += 1;
        if sll.sll_family != libc::AF_PACKET as u16 || sll.sll_ifindex != ifindex as i32 {
            seen.bad_sll.push(format!(
                "family {} ifindex {}",
                sll.sll_family, sll.sll_ifindex
            ));
        }
        if frame.len() >= 34 && frame[12] == 0x08 && frame[13] == 0x00 && frame[14] >> 4 == 4 {
            seen.ipv4 += 1;
            let src = Ipv4Addr::new(frame[26], frame[27], frame[28], frame[29]);
            let dst = Ipv4Addr::new(frame[30], frame[31], frame[32], frame[33]);
            let proto = frame[23];
            let involves_us = src == local || dst == local || src == peer || dst == peer;
            if involves_us {
                seen.ours += 1;
                if proto == 1 {
                    seen.icmp += 1;
                } else if proto != 17 {
                    seen.other_proto += 1;
                }
            }
        }
        // enough evidence: our datagrams and the echo went by
        if seen.ours >= 3 && seen.icmp >= 1 {
            break;
        }
    }
    seen
}

fn main() {
    let mut report = Report::new();

    let fd = unsafe {
        libc::socket(
            libc::AF_PACKET,
            libc::SOCK_RAW,
            (libc::ETH_P_ALL as u16).to_be() as i32,
        )
    };
    if fd < 0 {
        let e = errno();
        let no_way = [
            libc::EPERM,
            libc::EACCES,
            libc::EAFNOSUPPORT,
            libc::EPROTONOSUPPORT,
            libc::ESOCKTNOSUPPORT,
            libc::EPFNOSUPPORT,
            libc::EINVAL,
            libc::ENOSYS,
        ];
        if no_way.contains(&e) {
            println!("packet sniff skipped: socket(AF_PACKET) -> {}", os_err(e));
            return;
        }
        panic!("socket(AF_PACKET): {}", os_err(e));
    }
    println!("socket(AF_PACKET, SOCK_RAW, ETH_P_ALL) = {fd}");

    // the interface: named, or the first one that takes a bind
    let mut bound = None;
    let mut tried = Vec::new();
    if let Ok(name) = std::env::var("PACKET_IFACE") {
        let c = std::ffi::CString::new(name.clone()).unwrap();
        let idx = unsafe { libc::if_nametoindex(c.as_ptr()) };
        match bind_packet(fd, idx) {
            Ok(()) => bound = Some((idx, name)),
            Err(e) => tried.push(format!("{name}({idx}): {}", os_err(e))),
        }
    } else {
        for (idx, name) in interfaces() {
            match bind_packet(fd, idx) {
                Ok(()) => {
                    bound = Some((idx, name));
                    break;
                }
                Err(e) => tried.push(format!("{name}({idx}): {}", os_err(e))),
            }
        }
    }
    let (ifindex, ifname) = match bound {
        Some(b) => b,
        None => {
            // a host without packet sockets may still hand out a socket for
            // family 17 (it is AF_ROUTE on the BSDs) and only balk at the bind;
            // Windows has the emulation and must not get away with that
            let windows = std::env::var("OS").as_deref() == Ok("Windows_NT");
            if !windows {
                println!(
                    "packet sniff skipped: no interface took the bind: {}",
                    tried.join("; ")
                );
                return;
            }
            report.check(
                "bind",
                Err(format!("no interface took the bind: {}", tried.join("; "))),
            );
            report.finish("packet sniff ok");
            return;
        }
    };
    report.check("bind", Ok(format!("{ifname} (ifindex {ifindex})")));

    // promiscuous mode, which a sniffer asks for as a matter of course
    let mut mreq: libc::packet_mreq = unsafe { mem::zeroed() };
    mreq.mr_ifindex = ifindex as i32;
    mreq.mr_type = libc::PACKET_MR_PROMISC as u16;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_PACKET,
            libc::PACKET_ADD_MEMBERSHIP,
            &mreq as *const libc::packet_mreq as *const libc::c_void,
            mem::size_of::<libc::packet_mreq>() as libc::socklen_t,
        )
    };
    report.check(
        "promisc",
        if rc == 0 {
            Ok("PACKET_ADD_MEMBERSHIP accepted".into())
        } else {
            Err(os_err(errno()))
        },
    );

    // nonblocking, so a dry socket answers EAGAIN instead of hanging
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }

    let (local, guessed_peer) =
        local_and_gateway().unwrap_or((Ipv4Addr::UNSPECIFIED, Ipv4Addr::UNSPECIFIED));
    let peer: Ipv4Addr = std::env::var("PACKET_PEER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(guessed_peer);
    println!("local {local}, peer {peer}");

    make_traffic(peer);
    let seen = capture(fd, ifindex, local, peer, Duration::from_secs(4));
    println!(
        "frames {} ipv4 {} involving us {} icmp {} other {}",
        seen.frames, seen.ipv4, seen.ours, seen.icmp, seen.other_proto
    );

    report.check(
        "frames",
        if seen.frames > 0 {
            Ok(format!("{} frames", seen.frames))
        } else {
            Err("nothing came back in 4s".into())
        },
    );
    report.check(
        "ethernet-framing",
        if seen.ipv4 > 0 {
            Ok(format!("{} IPv4 frames with ethertype 0x0800", seen.ipv4))
        } else {
            Err("no frame parsed as Ethernet + IPv4".into())
        },
    );
    report.check(
        "own-traffic",
        if seen.ours > 0 {
            Ok(format!("{} frames between {local} and {peer}", seen.ours))
        } else {
            Err("none of the frames involved this host".into())
        },
    );
    report.check(
        "icmp-echo",
        if seen.icmp > 0 {
            Ok(format!("{} ICMP frames", seen.icmp))
        } else {
            Err("the ping to the peer never showed up".into())
        },
    );
    report.check(
        "sockaddr_ll",
        if seen.bad_sll.is_empty() {
            Ok(format!(
                "every frame reported AF_PACKET on ifindex {ifindex}"
            ))
        } else {
            Err(format!(
                "{} frames off: {}",
                seen.bad_sll.len(),
                seen.bad_sll[0]
            ))
        },
    );

    // a dry nonblocking socket says EAGAIN, not EOF
    let mut byte = [0u8; 2048];
    let mut dry = None;
    for _ in 0..2000 {
        let r = unsafe { libc::recv(fd, byte.as_mut_ptr() as *mut libc::c_void, byte.len(), 0) };
        if r < 0 {
            dry = Some(errno());
            break;
        }
    }
    report.check(
        "dry-eagain",
        match dry {
            Some(e) if e == libc::EAGAIN => Ok("EAGAIN once drained".into()),
            Some(e) => Err(os_err(e)),
            None => Err("2000 reads and never dry".into()),
        },
    );

    unsafe { libc::close(fd) };

    // the cooked form: no link header, the IP packet starts the buffer;
    // and read(), which a sniffer built on plain file I/O uses
    let dg = unsafe {
        libc::socket(
            libc::AF_PACKET,
            libc::SOCK_DGRAM,
            (libc::ETH_P_ALL as u16).to_be() as i32,
        )
    };
    report.check(
        "cooked-socket",
        if dg >= 0 {
            Ok(format!("fd {dg}"))
        } else {
            Err(os_err(errno()))
        },
    );
    if dg >= 0 {
        report.check(
            "cooked-bind",
            bind_packet(dg, ifindex)
                .map(|_| format!("ifindex {ifindex}"))
                .map_err(os_err),
        );
        make_traffic(peer);
        let mut got_ip = 0;
        let mut got_read = 0;
        let mut buf = [0u8; 65536];
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(4) && (got_ip == 0 || got_read == 0) {
            let mut pfd = libc::pollfd {
                fd: dg,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut pfd, 1, 500) } <= 0 {
                continue;
            }
            let r = if got_read == 0 {
                unsafe { libc::read(dg, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) }
            } else {
                unsafe { libc::recv(dg, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) }
            };
            if r <= 0 {
                continue;
            }
            let frame = &buf[..r as usize];
            if frame.len() >= 20 && frame[0] >> 4 == 4 {
                if got_read == 0 {
                    got_read += 1;
                }
                got_ip += 1;
            }
        }
        report.check(
            "cooked-framing",
            if got_ip > 0 {
                Ok(format!("{got_ip} packets began with an IPv4 header"))
            } else {
                Err("no cooked packet in 4s".into())
            },
        );
        report.check(
            "read",
            if got_read > 0 {
                Ok("read() handed back a packet".into())
            } else {
                Err("read() never produced one".into())
            },
        );
        unsafe { libc::close(dg) };
    }

    // open and close many times: nothing leaks a slot or a handle
    let mut reopened = 0;
    for _ in 0..40 {
        let s = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_RAW, 0) };
        if s < 0 {
            break;
        }
        unsafe { libc::close(s) };
        reopened += 1;
    }
    report.check(
        "reopen",
        if reopened == 40 {
            Ok("40 open/close cycles".into())
        } else {
            Err(format!("stopped after {reopened}: {}", os_err(errno())))
        },
    );

    report.finish("packet sniff ok");
}
