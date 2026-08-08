//! File I/O on smol: write, read back, copy and metadata, all async.

use smol::io::{AsyncBufReadExt, AsyncWriteExt};
use smol::stream::StreamExt;
use std::path::PathBuf;

const LINES: [&str; 4] = ["foo", "bar", "alice", "bob"];

fn main() {
    smol::block_on(async {
        let path = scratch_path();
        println!("scratch file: {}", path.display());

        let mut f = smol::fs::File::create(&path).await.expect("create");
        for line in LINES {
            f.write_all(format!("{line}\n").as_bytes()).await.expect("write");
        }
        f.flush().await.expect("flush");
        drop(f);

        let md = smol::fs::metadata(&path).await.expect("metadata");
        assert!(md.is_file(), "should be a regular file");
        println!("wrote {} bytes", md.len());

        // Read the whole thing back.
        let whole = smol::fs::read_to_string(&path).await.expect("read_to_string");
        let expected: String = LINES.iter().map(|l| format!("{l}\n")).collect();
        assert_eq!(whole, expected, "content differs from what we wrote");

        // Then again as a stream of lines.
        let f = smol::fs::File::open(&path).await.expect("open");
        let mut lines = smol::io::BufReader::new(f).lines();
        let mut got = Vec::new();
        while let Some(line) = lines.next().await {
            let line = line.expect("read line");
            println!("  {line}");
            got.push(line);
        }
        assert_eq!(got, LINES, "line stream differs from what we wrote");

        // Copy, then remove both.
        let copy = path.with_extension("copy");
        let n = smol::fs::copy(&path, &copy).await.expect("copy");
        assert_eq!(n, md.len(), "copy is a different size");
        println!("copied {n} bytes to {}", copy.display());

        smol::fs::remove_file(&copy).await.expect("remove copy");
        smol::fs::remove_file(&path).await.expect("remove original");
        assert!(!path.exists(), "file outlived remove_file");

        println!("\nsmol file io ok: {} lines", LINES.len());
    });
}

fn scratch_path() -> PathBuf {
    std::env::temp_dir().join(format!("rust-ape-async-io-{}.txt", std::process::id()))
}
