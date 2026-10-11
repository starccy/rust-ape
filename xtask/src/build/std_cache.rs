//! Keeping the build-std cache and the linked outputs in step with the
//! SDK, the host-libs metadata and the rewrite's version.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::toolchain::config::{self, TARGETS};
use crate::util;

/// Identity of everything std is built from: the library and libc stamps.
pub(super) fn std_stamp(root: &Path) -> Result<String> {
    let f = root.join("vendor/.stamps/libc");
    let libc = fs::read_to_string(&f)
        .with_context(|| format!("missing {}: run `cargo xtask setup` first", f.display()))?;
    Ok(format!("{}\n{}", config::rust_channel()?, libc.trim()))
}

// Cargo doesn't know the link reads the shim and the SDK's libraries, so
// every linked program is dropped when any of those files changed since
// the last build, by name, size and mtime recorded in a stamp rather
// than by comparing mtimes with the outputs: a repackaged SDK can carry
// mtimes older than a program linked against the previous one
pub(super) fn purge_stale_shim_links(root: &Path, target_dir: &Path, profile: &str) -> Result<()> {
    let mut stamp = String::new();
    fn walk(dir: &Path, stamp: &mut String) -> Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(&path, stamp)?;
            } else {
                let m = entry.metadata()?;
                let t = m
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                use std::fmt::Write;
                let _ = writeln!(stamp, "{} {} {t}", path.display(), m.len());
            }
        }
        Ok(())
    }
    walk(&root.join("shim"), &mut stamp)?;
    for &(_, arch) in TARGETS {
        let lib = root.join(format!("vendor/cosmocc/{arch}-linux-cosmo/lib"));
        if lib.is_dir() {
            walk(&lib, &mut stamp)?;
        }
    }
    let marker = target_dir
        .join("ape")
        .join(format!(".link-stamp-{profile}"));
    if fs::read_to_string(&marker).ok().as_deref() == Some(stamp.as_str()) {
        return Ok(());
    }
    for &(triple, _) in TARGETS {
        purge_older_linked(
            &target_dir.join(triple).join(profile),
            std::time::SystemTime::now(),
        )?;
    }
    fs::create_dir_all(marker.parent().unwrap())?;
    fs::write(&marker, stamp)?;
    Ok(())
}

pub(super) fn purge_older_linked(dir: &Path, newer_than: std::time::SystemTime) -> Result<()> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in rd {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            purge_older_linked(&path, newer_than)?;
        } else if path.to_string_lossy().ends_with(".com.dbg")
            && entry.metadata()?.modified()? <= newer_than
        {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

// The windows-* proc macros are host units, which RUSTFLAGS and its
// version cfg don't reach, so their artifacts are dropped by hand when
// the rewrite changes (see host/rustcwrap/windows_macro.rs).
pub(super) fn purge_windows_macros(target_dir: &Path, profile: &str) -> Result<()> {
    let stamp = target_dir.join("ape").join(format!(".rustcwrap-{profile}"));
    let version = crate::host::rustcwrap::VERSION.to_string();
    if fs::read_to_string(&stamp).ok().as_deref() == Some(version.as_str()) {
        return Ok(());
    }
    let host = target_dir.join(profile);
    for (sub, prefix) in [
        ("deps", "libwindows_"),
        ("deps", "windows_"),
        (".fingerprint", "windows-"),
    ] {
        let Ok(rd) = fs::read_dir(host.join(sub)) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name();
            if name.to_string_lossy().starts_with(prefix) {
                let p = e.path();
                if p.is_dir() {
                    fs::remove_dir_all(&p)?;
                } else {
                    fs::remove_file(&p)?;
                }
            }
        }
    }
    fs::create_dir_all(stamp.parent().unwrap())?;
    fs::write(&stamp, version)?;
    Ok(())
}

pub(super) fn clean_std(target_dir: &Path, profile: &str) -> Result<()> {
    for &(triple, _) in TARGETS {
        let base = target_dir.join(triple).join(profile);

        remove_prefixed(
            &base.join(".fingerprint"),
            &["std-", "panic_abort-", "libc-"],
        )?;
        remove_prefixed(&base.join("deps"), &["liblibc-", "libc-"])?;
    }
    Ok(())
}

fn remove_prefixed(dir: &Path, prefixes: &[&str]) -> Result<()> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in rd {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if prefixes.iter().any(|p| name.starts_with(p)) {
            let path = entry.path();
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

/// The std that was just built has to call into the shim. If the libc
/// patch was dropped from the build-std graph for any reason, std still
/// links against cosmo's raw sigaction and the layout bug behind
/// shim/signal.c comes back without a word from cargo, so the rlib is
/// checked for the one redirect that matters most.
pub(super) fn verify_std_redirects(target_dir: &Path, profile: &str, triple: &str) -> Result<()> {
    let deps = target_dir.join(triple).join(profile).join("deps");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(&deps)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with("libstd-") && name.ends_with(".rlib") {
            let m = fs::metadata(&path)?.modified()?;
            if newest.as_ref().is_none_or(|(t, _)| *t < m) {
                newest = Some((m, path));
            }
        }
    }
    let Some((_, rlib)) = newest else {
        bail!("no libstd rlib under {}", deps.display());
    };
    // rustup's llvm-nm (the llvm-tools component pinned in rust-toolchain.toml)
    // reads the rlib whether it holds objects or, under an LTO profile,
    // bitcode; GNU nm would list nothing for the latter.
    let sysroot = util::capture(Command::new("rustc").arg("--print").arg("sysroot"))?;
    let rustlib = PathBuf::from(sysroot.trim()).join("lib/rustlib");
    let llvm_nm = fs::read_dir(&rustlib)?
        .flatten()
        .map(|e| e.path().join("bin/llvm-nm"))
        .find(|p| p.is_file());
    let Some(llvm_nm) = llvm_nm else {
        bail!(
            "llvm-nm not found under {}; `rustup component add llvm-tools`",
            rustlib.display()
        );
    };
    let out = Command::new(llvm_nm)
        .arg("-u")
        .arg(&rlib)
        .output()
        .context("running llvm-nm")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let imported: std::collections::BTreeSet<&str> = text
        .lines()
        .filter_map(|l| l.split_whitespace().last())
        .filter(|s| s.starts_with("__ape_shim_"))
        .collect();
    if !imported.contains("__ape_shim_sigaction") {
        bail!(
            "{} does not call __ape_shim_sigaction, so std was built against an unpatched libc \
             (shim symbols it does call: {:?}).\n\
             Check that `cargo build` did not warn that the libc patch was unused, then \
             `cargo xtask build --clean-std`",
            rlib.display(),
            imported
        );
    }
    Ok(())
}
