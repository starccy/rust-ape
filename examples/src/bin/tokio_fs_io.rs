//! tokio::fs and the AsyncRead/AsyncWrite combinators.

use std::time::Duration;

use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};

const LINES: &[&str] = &["first", "second", "third", "fourth"];

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        let dir = std::env::temp_dir().join(format!("rust-ape-tokio-{}", std::process::id()));
        fs::create_dir_all(&dir).await.expect("create_dir_all");

        files(&dir).await;
        directories(&dir).await;
        combinators(&dir).await;
        duplex().await;
        concurrent(&dir).await;

        fs::remove_dir_all(&dir).await.expect("remove_dir_all");
        assert!(fs::metadata(&dir).await.is_err(), "directory survived removal");
    });

    println!("\ntokio fs io ok");
}

async fn files(dir: &std::path::Path) {
    let path = dir.join("plain.txt");

    fs::write(&path, b"written whole").await.expect("fs::write");
    assert_eq!(fs::read(&path).await.expect("fs::read"), b"written whole");
    assert_eq!(fs::read_to_string(&path).await.expect("read_to_string"), "written whole");

    let md = fs::metadata(&path).await.expect("metadata");
    assert!(md.is_file());
    assert_eq!(md.len(), 13);

    // Append, then seek back over what was there before.
    let mut f = fs::OpenOptions::new().append(true).open(&path).await.expect("open append");
    f.write_all(b" and more").await.expect("append");
    f.flush().await.expect("flush");
    drop(f);

    let mut f = fs::File::open(&path).await.expect("open");
    f.seek(std::io::SeekFrom::Start(13)).await.expect("seek");
    let mut tail = String::new();
    f.read_to_string(&mut tail).await.expect("read tail");
    assert_eq!(tail, " and more", "seek landed somewhere else");

    let renamed = dir.join("renamed.txt");
    fs::rename(&path, &renamed).await.expect("rename");
    assert!(fs::metadata(&path).await.is_err(), "old name still resolves");
    fs::remove_file(&renamed).await.expect("remove_file");

    println!("files: write, read, append, seek, rename, remove");
}

async fn directories(dir: &std::path::Path) {
    let nested = dir.join("a").join("b");
    fs::create_dir_all(&nested).await.expect("create_dir_all nested");
    for i in 0..5 {
        fs::write(nested.join(format!("f{i}")), format!("{i}")).await.expect("write child");
    }

    let mut names = Vec::new();
    let mut entries = fs::read_dir(&nested).await.expect("read_dir");
    while let Some(e) = entries.next_entry().await.expect("next_entry") {
        assert!(e.file_type().await.expect("file_type").is_file());
        names.push(e.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    assert_eq!(names, ["f0", "f1", "f2", "f3", "f4"], "read_dir gave {names:?}");

    fs::remove_dir_all(dir.join("a")).await.expect("remove_dir_all");
    println!("directories: created nested, listed 5 entries, removed the tree");
}

async fn combinators(dir: &std::path::Path) {
    let src = dir.join("lines.txt");
    let mut f = fs::File::create(&src).await.expect("create");
    for line in LINES {
        f.write_all(format!("{line}\n").as_bytes()).await.expect("write line");
    }
    f.flush().await.expect("flush");
    drop(f);

    let mut got = Vec::new();
    let mut reader = BufReader::new(fs::File::open(&src).await.expect("open")).lines();
    while let Some(line) = reader.next_line().await.expect("next_line") {
        got.push(line);
    }
    assert_eq!(got, LINES, "BufReader::lines gave {got:?}");

    // io::copy between two files, through the blocking pool on both ends.
    let dst = dir.join("copy.txt");
    let mut r = fs::File::open(&src).await.expect("open src");
    let mut w = fs::File::create(&dst).await.expect("create dst");
    let n = tokio::io::copy(&mut r, &mut w).await.expect("copy");
    w.flush().await.expect("flush dst");
    assert_eq!(n as usize, LINES.iter().map(|l| l.len() + 1).sum::<usize>());
    assert_eq!(
        fs::read_to_string(&dst).await.expect("read dst"),
        fs::read_to_string(&src).await.expect("read src")
    );

    // take() and chain() on top of a file.
    let mut head = String::new();
    fs::File::open(&src)
        .await
        .expect("open")
        .take(5)
        .read_to_string(&mut head)
        .await
        .expect("take");
    assert_eq!(head, "first");

    let mut joined = String::new();
    (&b"before "[..])
        .chain(&b"after"[..])
        .read_to_string(&mut joined)
        .await
        .expect("chain");
    assert_eq!(joined, "before after");

    fs::remove_file(&src).await.expect("remove src");
    fs::remove_file(&dst).await.expect("remove dst");
    println!("combinators: lines, copy, take, chain");
}

/// duplex() is an in-memory socket pair, so split/join work without touching
/// the OS at all.
async fn duplex() {
    let (a, b) = tokio::io::duplex(64);
    let (mut ar, mut aw) = tokio::io::split(a);
    let (mut br, mut bw) = tokio::io::split(b);

    let echo = tokio::spawn(async move {
        let mut buf = [0u8; 32];
        let n = br.read(&mut buf).await.expect("duplex read");
        bw.write_all(&buf[..n]).await.expect("duplex write");
        bw.shutdown().await.expect("shutdown");
    });

    aw.write_all(b"round trip").await.expect("write");
    let mut back = [0u8; 10];
    ar.read_exact(&mut back).await.expect("read_exact");
    assert_eq!(&back, b"round trip");
    echo.await.expect("echo task");
    println!("duplex: split, echoed and shut down");
}

/// Thirty-two files at once, which is what actually loads the blocking pool.
async fn concurrent(dir: &std::path::Path) {
    let tasks: Vec<_> = (0..32usize)
        .map(|i| {
            let path = dir.join(format!("c{i}"));
            tokio::spawn(async move {
                let body = format!("file {i}").repeat(64);
                fs::write(&path, &body).await.expect("write");
                tokio::time::sleep(Duration::from_millis(5)).await;
                let back = fs::read_to_string(&path).await.expect("read");
                assert_eq!(back, body, "file {i} came back wrong");
                fs::remove_file(&path).await.expect("remove");
                body.len()
            })
        })
        .collect();
    let mut total = 0;
    for t in tasks {
        total += t.await.expect("task");
    }
    println!("concurrent: 32 files written and read back, {total} bytes");
}
