// Test data some examples carry, made at build time with the build
// machine's tools:
//
// - bundle/rust-ape-host-data/cities.db from data/cities.sql, with sqlite3
//   or python3; bundle.toml packs it for host_sqlite
// - testsrc.mp4 in OUT_DIR, a one second test pattern from ffmpeg, which
//   host_ffmpeg includes
//
// A tool that's missing is reported as a warning, the file is left out
// (the clip left empty), and the example skips the part that needs it.
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let sql = manifest.join("data/cities.sql");
    println!("cargo:rerun-if-changed={}", sql.display());
    let dir = manifest.join("bundle/rust-ape-host-data");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("cities.db");
    let _ = std::fs::remove_file(&db);
    if let Err(e) = make_db(&sql, &db) {
        let _ = std::fs::remove_file(&db);
        println!("cargo:warning=no cities.db for host_sqlite, it skips the bundled database: {e}");
    }

    let clip = out.join("testsrc.mp4");
    if !clip.is_file() || std::fs::metadata(&clip).unwrap().len() == 0 {
        if let Err(e) = make_clip(&clip) {
            std::fs::write(&clip, b"").unwrap();
            println!("cargo:warning=no test clip for host_ffmpeg, it skips decoding: {e}");
        }
    }
}

fn make_db(sql: &Path, db: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(sql).map_err(|e| e.to_string())?;
    let cli = run(Command::new("sqlite3").arg(db).arg(&text));
    if cli.is_ok() {
        return cli;
    }
    let script = "import sqlite3, sys\n\
                  c = sqlite3.connect(sys.argv[1])\n\
                  c.executescript(sys.argv[2])\n\
                  c.commit()\n";
    run(Command::new("python3")
        .args(["-c", script])
        .arg(db)
        .arg(&text))
    .map_err(|e| format!("{} and {e}", cli.unwrap_err()))
}

fn make_clip(clip: &Path) -> Result<(), String> {
    let encode = |codec: &str| {
        run(Command::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=320x240:rate=30",
            ])
            .args(["-c:v", codec, "-pix_fmt", "yuv420p"])
            .args(["-fflags", "+bitexact", "-map_metadata", "-1"])
            .arg(clip))
    };
    // H.264 when this ffmpeg has an encoder for it, MPEG-4 part 2 otherwise
    encode("libx264").or_else(|_| encode("mpeg4"))
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd
        .output()
        .map_err(|e| format!("{}: {e}", cmd.get_program().display()))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "{}: {}",
        cmd.get_program().display(),
        err.lines().last().unwrap_or("failed").trim()
    ))
}
