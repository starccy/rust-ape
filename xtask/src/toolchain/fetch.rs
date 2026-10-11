use crate::config::{self, PATCHED_CRATES};
use crate::util::{self, apply_patch, download, rm_rf, sha256_file, up_to_date, write_stamp};
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

/// Restores the two parts of vendor/: patched crates and cosmocc, each
/// idempotent on its own stamp. std itself comes from the toolchain's rust-src.
pub fn run(force: bool, local_cosmocc: Option<&Path>) -> Result<()> {
    let root = util::repo_root();
    let dirs = Dirs {
        patches: root.join("patches"),
        vendor: root.join("vendor"),
        cache: root.join("cache"),
        stamps: root.join("vendor/.stamps"),
    };
    config::check_std_libc()?;
    fetch_crates(&dirs, force)?;
    match local_cosmocc {
        Some(dir) => link_cosmocc(&dirs, dir)?,
        None => fetch_cosmocc(&dirs, force)?,
    }
    println!("ready: {}", dirs.vendor.display());
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
        util::run(
            Command::new("tar")
                .arg("-xzf")
                .arg(&cratefile)
                .arg("-C")
                .arg(&crates_dir),
        )?;
        fs::rename(&extracted, &dir)?;
        apply_patch(&crates_dir, &patch)?;
        write_stamp(&stamp_file, &stamp)?;
    }

    for entry in fs::read_dir(&crates_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !PATCHED_CRATES.iter().any(|&(n, _)| n == name) {
            println!("==> removing stale vendor/patches/{name}");
            rm_rf(&entry.path())?;
            match fs::remove_file(dirs.stamps.join(&*name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
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
    util::run(
        Command::new("unzip")
            .arg("-q")
            .arg(&zip)
            .arg("-d")
            .arg(&dir),
    )?;
    write_stamp(&stamp_file, &stamp)?;
    Ok(())
}

fn link_cosmocc(dirs: &Dirs, local: &Path) -> Result<()> {
    let local = local
        .canonicalize()
        .with_context(|| format!("no such toolchain directory: {}", local.display()))?;
    for rel in [
        "bin/cosmocc",
        "include/libc",
        "x86_64-linux-cosmo/lib",
        "aarch64-linux-cosmo/lib",
    ] {
        if !local.join(rel).exists() {
            bail!(
                "{} does not look like a cosmocc tree (missing {rel}); point --cosmocc at the output of tool/cosmocc/package.sh",
                local.display()
            );
        }
    }
    let dir = dirs.vendor.join("cosmocc");
    let stamp_file = dirs.stamps.join("cosmocc");
    let stamp = format!("local:{}", local.display());
    if fs::read_link(&dir).ok().as_deref() == Some(&*local) && up_to_date(&dir, &stamp_file, &stamp)
    {
        println!("==> cosmocc already linked to {}", local.display());
        return Ok(());
    }
    rm_rf(&dir)?;
    fs::create_dir_all(&dirs.vendor)?;
    std::os::unix::fs::symlink(&local, &dir)?;
    println!("==> cosmocc -> {}", local.display());
    write_stamp(&stamp_file, &stamp)?;
    Ok(())
}
