//! Unix-spelled paths in a child's argv, on NT.
//!
//! An APE child must see every argument exactly as sent, `/x/...` shapes
//! included, and a directory named through a letter that is no drive must
//! still resolve for it. A native child must see `/x/...` rewritten to
//! `x:\...`, but only when the letter is an existing drive. Off NT nothing
//! is rewritten, so the APE case doubles as a plain argv round trip.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use rust_ape_examples::Report;

fn e<T>(what: &str, r: Result<T, std::io::Error>) -> Result<T, String> {
    r.map_err(|err| format!("{what}: {err}"))
}

fn run(exe: &str, args: &[String]) -> Result<Vec<String>, String> {
    let out = e("spawn", Command::new(exe).args(args).output())?;
    if !out.status.success() {
        return Err(format!("child failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect())
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("--child") {
        // Print the arguments back, one per line; with --stat, what each
        // one names instead.
        let stat = argv.get(2).map(String::as_str) == Some("--stat");
        for a in &argv[if stat { 3 } else { 2 }..] {
            if stat {
                println!("{}", match fs::metadata(a) {
                    Ok(m) if m.is_dir() => "dir",
                    Ok(_) => "file",
                    Err(_) => "missing",
                });
            } else {
                println!("{a}");
            }
        }
        return;
    }

    let mut rep = Report::new();
    let exe = ape::program_executable_name().expect("program_executable_name");
    let pid = std::process::id();
    let nt = ape::is_windows();

    // A letter with no drive behind it. Off NT any letter will do, since
    // nothing is rewritten there.
    let free = if nt {
        let drives: Vec<String> = fs::read_dir("/")
            .map(|d| d.flatten().map(|d| d.file_name().to_string_lossy().to_ascii_uppercase()).filter(|n| n.len() == 1).collect())
            .unwrap_or_default();
        ('A'..='Z').rev().find(|c| !drives.contains(&c.to_string())).unwrap_or('Q')
    } else {
        'Q'
    }
    .to_ascii_lowercase();
    let taken = if nt {
        std::env::var("SYSTEMDRIVE").ok().and_then(|d| d.chars().next()).unwrap_or('C')
    } else {
        'C'
    }
    .to_ascii_lowercase();
    println!("no drive: {free}  drive: {taken}");

    rep.check("ape-child-verbatim", (|| {
        let args: Vec<String> = [
            format!("/{free}/"),
            format!("/{free}/f.txt"),
            format!("/{free}"),
            format!("/{taken}/"),
            format!("/{taken}/Windows"),
            format!("/{free}/a:/{free}/b"),
            "/tmp/x".to_string(),
            "a/b".to_string(),
        ]
        .into_iter()
        .collect();
        let mut full = vec!["--child".to_string()];
        full.extend(args.iter().cloned());
        let got = run(&exe, &full)?;
        if got != args {
            return Err(format!("sent {args:?}, child saw {got:?}"));
        }
        Ok(format!("{} arguments came back as sent", args.len()))
    })());

    if nt {
        // The bug as seen from a shell: a directory under the cosmos drive,
        // named through a letter that is no drive, must still be there for
        // the child.
        rep.check("ape-child-no-drive-dir", (|| {
            let dir = PathBuf::from(format!("/{free}/spawn_argv-{pid}"));
            e("mkdir", fs::create_dir_all(&dir))?;
            let file = dir.join("f.txt");
            e("write", fs::write(&file, "x"))?;
            let r = (|| {
                let args = vec![
                    "--child".to_string(),
                    "--stat".to_string(),
                    format!("{}/", dir.display()),
                    file.display().to_string(),
                    format!("/{free}"),
                ];
                let got = run(&exe, &args)?;
                let want = ["dir", "file", "dir"];
                if got != want {
                    return Err(format!("child saw {got:?}, wanted {want:?}"));
                }
                Ok(format!("{} and its file resolve in the child", dir.display()))
            })();
            let _ = fs::remove_dir_all(&dir);
            let _ = fs::remove_dir(format!("/{free}"));
            r
        })());

        // A native child gets the rewrite, and only for a real drive.
        rep.check("native-child-rewrite", (|| {
            let cmd = format!("/{taken}/Windows/System32/cmd.exe");
            let args = vec![
                "/c".to_string(),
                "echo".to_string(),
                format!("/{taken}/x"),
                format!("/{free}/x"),
                "/tmp/x".to_string(),
            ];
            let got = run(&cmd, &args)?;
            let want = format!("{taken}:\\x /{free}/x /tmp/x");
            if got.first().map(|s| s.trim()) != Some(want.as_str()) {
                return Err(format!("cmd echoed {got:?}, wanted {want:?}"));
            }
            Ok(format!("cmd.exe saw {want:?}"))
        })());
    }

    rep.finish("spawn argv paths ok");
}
