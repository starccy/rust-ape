// Builds the test library host_layout loads from data/layout-test/lp.c.
// One copy per host goes to bundle/rust-ape-layout-test, which
// bundle.toml packs into the zip store; another, for this machine, goes
// to OUT_DIR under the name `-l lp` resolves to, so the link sees it the
// way it sees any library a -sys crate asks for.
//
// The host copies need clang and lld. One that can't be built is
// reported as a warning and left out, and host_layout skips that host.
// Without a C library to link, the aarch64 Linux copy gets libgcc_s.so.1
// as a dependency by name, through an empty stand-in, for the binary128
// arithmetic its long double needs.
use std::path::{Path, PathBuf};
use std::process::Command;

// clang's target, file name, extra flags
const HOSTS: &[(&str, &str, &[&str])] = &[
    ("x86_64-linux-gnu", "liblp-x86_64.so", &["-fPIC", "-s"]),
    ("aarch64-linux-gnu", "liblp-aarch64.so", &["-fPIC", "-s"]),
    ("arm64-apple-macos11", "liblp.dylib", &[]),
    // lp_apply_long and lp_wchar_ret are written in assembly there
    (
        "x86_64-pc-windows-msvc",
        "lp.dll",
        &[
            "-Wl,/noentry",
            "-Wl,/export:lp_apply_long",
            "-Wl,/export:lp_wchar_ret",
            "-Wno-dll-attribute-on-redeclaration",
        ],
    ),
];

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("../data/layout-test");
    let lp_c = src.join("lp.c");
    println!("cargo:rerun-if-changed={}", lp_c.display());
    println!("cargo:rerun-if-changed={}", src.join("lp.h").display());

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let link = out.join("liblp.so");
    let _ = std::fs::remove_file(&link);
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    if let Err(e) = run(Command::new(&cc)
        .args(["-O1", "-shared", "-fPIC", "-o"])
        .arg(&link)
        .arg(&lp_c))
    {
        panic!("building the test library with {cc} for the link: {e}");
    }
    println!("cargo:rustc-link-search=native={}", out.display());

    let dir = manifest.join("../bundle/rust-ape-layout-test");
    std::fs::create_dir_all(&dir).unwrap();
    for &(target, name, extra) in HOSTS {
        let dst = dir.join(name);
        let _ = std::fs::remove_file(&dst);
        let mut extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        let built = if target == "aarch64-linux-gnu" {
            libgcc_stub(target, &out).map(|stub| {
                extra.push(format!("-L{}", stub.display()));
                extra.extend(["-Wl,--no-as-needed".into(), "-l:libgcc_s.so.1".into()]);
            })
        } else {
            Ok(())
        };
        if let Err(e) = built.and_then(|()| clang(target, &extra, &lp_c, &dst)) {
            println!("cargo:warning=no {name} for host_layout, it skips that host: {e}");
        }
    }
    for leftover in ["lp.lib", "lp.pdb"] {
        let _ = std::fs::remove_file(dir.join(leftover));
    }
}

// an empty libgcc_s.so.1 for `target`, in a directory of its own under out
fn libgcc_stub(target: &str, out: &Path) -> Result<PathBuf, String> {
    let dir = out.join(format!("libgcc-{target}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let empty = dir.join("empty.c");
    std::fs::write(&empty, "").map_err(|e| e.to_string())?;
    let flags = ["-Wl,-soname,libgcc_s.so.1".to_string()];
    clang(target, &flags, &empty, &dir.join("libgcc_s.so.1"))?;
    Ok(dir)
}

fn clang(target: &str, extra: &[String], src: &Path, dst: &Path) -> Result<(), String> {
    run(Command::new("clang")
        .arg(format!("--target={target}"))
        .args([
            "-O1",
            "-shared",
            "-nostdlib",
            "-ffreestanding",
            "-fno-stack-protector",
            "-fuse-ld=lld",
        ])
        .args(extra)
        .arg("-o")
        .arg(dst)
        .arg(src))
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd
        .output()
        .map_err(|e| format!("{}: {e}", cmd.get_program().display()))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(err.trim().to_string())
}
