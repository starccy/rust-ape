//! `std::io::copy` between the fd kinds std has kernel-side fast paths for.
//!
//! On a linux target std picks `copy_file_range` for file to file,
//! `sendfile` for file to pipe or socket, and `splice` when a pipe is on
//! either end, and it probes each one with the errno cosmo hands back.
//! Every pairing here has to land on the right bytes on every host,
//! whichever path cosmo takes underneath.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

const SIZE: usize = 3 * 1024 * 1024 + 12345;

fn payload() -> Vec<u8> {
    (0..SIZE)
        .map(|i| (i.wrapping_mul(31) ^ (i >> 7)) as u8)
        .collect()
}

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("io_copy-{}-{name}", std::process::id()))
}

fn main() {
    // the child half: write the payload to stdout and leave
    if std::env::args().nth(1).as_deref() == Some("--child") {
        io::stdout().write_all(&payload()).unwrap();
        return;
    }
    let data = payload();
    let src_path = scratch("src");
    std::fs::write(&src_path, &data).expect("write source");

    // file -> file
    {
        let mut src = File::open(&src_path).unwrap();
        let dst_path = scratch("file");
        let mut dst = File::create(&dst_path).unwrap();
        let n = io::copy(&mut src, &mut dst).expect("file -> file");
        assert_eq!(n as usize, SIZE);
        drop(dst);
        assert_eq!(
            std::fs::read(&dst_path).unwrap(),
            data,
            "file -> file bytes"
        );
        std::fs::remove_file(&dst_path).unwrap();
        println!("file -> file ok");
    }

    // file -> file, from an offset, into a file that already has a tail
    {
        let mut src = File::open(&src_path).unwrap();
        src.seek(SeekFrom::Start(1000)).unwrap();
        let dst_path = scratch("offset");
        let mut dst = File::create(&dst_path).unwrap();
        dst.write_all(b"head").unwrap();
        let n = io::copy(&mut src, &mut dst).expect("file -> file from offset");
        assert_eq!(n as usize, SIZE - 1000);
        drop(dst);
        let got = std::fs::read(&dst_path).unwrap();
        assert_eq!(&got[..4], b"head");
        assert_eq!(&got[4..], &data[1000..], "offset copy bytes");
        std::fs::remove_file(&dst_path).unwrap();
        println!("file -> file from offset ok");
    }

    // file -> pipe, pipe -> file
    {
        let (mut reader, mut writer) = io::pipe().expect("pipe");
        let src_path2 = src_path.clone();
        let feeder = thread::spawn(move || {
            let mut src = File::open(&src_path2).unwrap();
            io::copy(&mut src, &mut writer).expect("file -> pipe")
        });
        let dst_path = scratch("pipe");
        let mut dst = File::create(&dst_path).unwrap();
        let n = io::copy(&mut reader, &mut dst).expect("pipe -> file");
        assert_eq!(n as usize, SIZE);
        assert_eq!(feeder.join().unwrap() as usize, SIZE);
        drop(dst);
        assert_eq!(std::fs::read(&dst_path).unwrap(), data, "pipe bytes");
        std::fs::remove_file(&dst_path).unwrap();
        println!("file -> pipe -> file ok");
    }

    // pipe -> pipe
    {
        let (mut r1, mut w1) = io::pipe().unwrap();
        let (mut r2, mut w2) = io::pipe().unwrap();
        let data2 = data.clone();
        let feeder = thread::spawn(move || w1.write_all(&data2).unwrap());
        let mover = thread::spawn(move || io::copy(&mut r1, &mut w2).expect("pipe -> pipe"));
        let mut got = Vec::new();
        r2.read_to_end(&mut got).unwrap();
        feeder.join().unwrap();
        assert_eq!(mover.join().unwrap() as usize, SIZE);
        assert_eq!(got, data, "pipe -> pipe bytes");
        println!("pipe -> pipe ok");
    }

    // file -> socket, socket -> file
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let src_path2 = src_path.clone();
        let sender = thread::spawn(move || {
            let mut sock = TcpStream::connect(addr).unwrap();
            let mut src = File::open(&src_path2).unwrap();
            let n = io::copy(&mut src, &mut sock).expect("file -> socket");
            sock.shutdown(std::net::Shutdown::Write).unwrap();
            n
        });
        let (mut conn, _) = listener.accept().unwrap();
        let dst_path = scratch("socket");
        let mut dst = File::create(&dst_path).unwrap();
        let n = io::copy(&mut conn, &mut dst).expect("socket -> file");
        assert_eq!(n as usize, SIZE);
        assert_eq!(sender.join().unwrap() as usize, SIZE);
        drop(dst);
        assert_eq!(std::fs::read(&dst_path).unwrap(), data, "socket bytes");
        std::fs::remove_file(&dst_path).unwrap();
        println!("file -> socket -> file ok");
    }

    // stdout of a child, through a pipe std owns, into a file
    {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--child")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn self");
        let mut out = child.stdout.take().unwrap();
        let mut got = Vec::new();
        let n = io::copy(&mut out, &mut got).expect("child pipe -> vec");
        assert!(child.wait().unwrap().success());
        assert_eq!(n as usize, SIZE);
        assert_eq!(got, data, "child stdout bytes");
        println!("child stdout -> memory ok");
    }

    std::fs::remove_file(&src_path).unwrap();
    println!("io_copy: all ok");
}
