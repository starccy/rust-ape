use crate::config::{self, PATCHED_CRATES};
use crate::util::{
    self, apply_patch, copy_dir, download, rm_rf, sha256_file, up_to_date, write_stamp,
};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Dirs {
    patches: PathBuf,
    vendor: PathBuf,
    cache: PathBuf,
    stamps: PathBuf,
}

/// Restores the three parts of vendor/: library, patched crates, cosmocc.
/// each idempotent on its own stamp
pub fn run(force: bool) -> Result<()> {
    let root = util::repo_root();
    let dirs = Dirs {
        patches: root.join("patches"),
        vendor: root.join("vendor"),
        cache: root.join("cache"),
        stamps: root.join("vendor/.stamps"),
    };
    if !dirs.patches.join("library.patch").is_file() {
        bail!("missing {}/library.patch", dirs.patches.display());
    }
    fetch_library(&dirs, force)?;
    fetch_crates(&dirs, force)?;
    fetch_cosmocc(&dirs, force)?;
    println!("ready: {}", dirs.vendor.display());
    Ok(())
}

/// vendor/library = rust-src for the pinned nightly + library.patch
fn fetch_library(dirs: &Dirs, force: bool) -> Result<()> {
    let channel = config::rust_channel()?;
    let patch = dirs.patches.join("library.patch");
    let dir = dirs.vendor.join("library");
    let stamp_file = dirs.stamps.join("library");
    
    let stamp = format!("{channel} patch:{}", sha256_file(&patch)?);
    if !force && up_to_date(&dir, &stamp_file, &stamp) {
        println!("==> library up to date, skipping");
        return Ok(());
    }
    rm_rf(&dir)?;
    fs::create_dir_all(&dirs.vendor)?;

    if let Some(src) = local_rust_src(channel) {
        let commit = toolchain_commit(channel).unwrap_or_else(|| "unknown".into());
        println!("==> copying rust-src from local rustup {channel} (commit {commit})");
        copy_dir(&src, &dir)?;
    } else {
        let tarball = dirs.cache.join(format!("rust-src-{channel}.tar.xz"));
        download(&config::rust_src_url()?, &tarball)?;
        println!("==> unpacking rust-src");
        let tmp = dirs.cache.join("rust-src-extract");
        rm_rf(&tmp)?;
        fs::create_dir_all(&tmp)?;
        util::run(Command::new("tar").arg("-xJf").arg(&tarball).arg("-C").arg(&tmp))?;
        let libdir = util::find_dir_suffix(&tmp, "rustlib/src/rust/library")
            .context("no library directory in the tarball")?;
        fs::rename(&libdir, &dir)?;
        rm_rf(&tmp)?;
    }
    apply_patch(&dirs.vendor, &patch)?;
    write_stamp(&stamp_file, &stamp)?;
    Ok(())
}

/// vendor/patches/<name> = pristine crates.io source + <name>.patch
fn fetch_crates(dirs: &Dirs, force: bool) -> Result<()> {
    let crates_dir = dirs.vendor.join("patches");
    fs::create_dir_all(&crates_dir)?;
    for &(name, ver) in PATCHED_CRATES {
        let patch = dirs.patches.join(format!("{name}.patch"));
        let dir = crates_dir.join(name);
        let stamp_file = dirs.stamps.join(name);
        let stamp = format!("{name}-{ver} patch:{}", sha256_file(&patch)?);
        if !force && up_to_date(&dir, &stamp_file, &stamp) {
            println!("==> {name} up to date, skipping");
            continue;
        }
        rm_rf(&dir)?;
        let extracted = crates_dir.join(format!("{name}-{ver}"));
        rm_rf(&extracted)?;

        let cratefile = dirs.cache.join(format!("{name}-{ver}.crate"));
        download(&config::crate_url(name, ver), &cratefile)?;
        util::run(Command::new("tar").arg("-xzf").arg(&cratefile).arg("-C").arg(&crates_dir))?;
        fs::rename(&extracted, &dir)?;
        apply_patch(&crates_dir, &patch)?;
        write_stamp(&stamp_file, &stamp)?;
    }
    Ok(())
}

/// vendor/cosmocc = the pinned official zip; the stamp carries the version, so
/// bumping it rebuilds by itself.
fn fetch_cosmocc(dirs: &Dirs, force: bool) -> Result<()> {
    let dir = dirs.vendor.join("cosmocc");
    let stamp_file = dirs.stamps.join("cosmocc");
    let stamp = format!(
        "cosmocc-{} sha256:{}",
        config::COSMOCC_VERSION,
        config::COSMOCC_SHA256
    );
    if !force && up_to_date(&dir, &stamp_file, &stamp) {
        println!("==> cosmocc up to date, skipping");
        return Ok(());
    }
    rm_rf(&dir)?;
    let zip = dirs
        .cache
        .join(format!("cosmocc-{}.zip", config::COSMOCC_VERSION));
    download(config::cosmocc_url(), &zip)?;
    let got = sha256_file(&zip)?;
    if got != config::COSMOCC_SHA256 {
        fs::remove_file(&zip)?;
        bail!(
            "cosmocc zip failed its SHA256 check, cached copy deleted — try again.\n  expected {}\n  got      {got}",
            config::COSMOCC_SHA256
        );
    }
    println!("==> unpacking cosmocc");
    fs::create_dir_all(&dir)?;
    util::run(Command::new("unzip").arg("-q").arg(&zip).arg("-d").arg(&dir))?;
    write_stamp(&stamp_file, &stamp)?;
    Ok(())
}

fn toolchain_commit(tc: &str) -> Option<String> {
    let out = util::capture(
        Command::new("rustc")
            .arg(format!("+{tc}"))
            .args(["--version", "--verbose"]),
    )
    .ok()?;
    out.lines()
        .find_map(|l| l.strip_prefix("commit-hash: ").map(str::to_owned))
}

/// This channel's rust-src in the local rustup, adding the component if needed.
fn local_rust_src(channel: &str) -> Option<PathBuf> {
    let sysroot = util::capture(
        Command::new("rustc")
            .arg(format!("+{channel}"))
            .args(["--print", "sysroot"]),
    )
    .ok()?;
    let src = Path::new(sysroot.trim()).join("lib/rustlib/src/rust/library");
    if !src.is_dir() {
        let added = Command::new("rustup")
            .args(["component", "add", "rust-src", "--toolchain", channel])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !added {
            return None;
        }
    }
    src.is_dir().then_some(src)
}
