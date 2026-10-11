//! The libc crate's side: its constant names and the values rustc gives
//! them for each target.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::ARCHES;
use crate::util;

/// Search order inside the libc crate for one arch. most specific first.
pub(super) fn libc_search_paths(root: &Path, arch: &str) -> Vec<PathBuf> {
    let src = root.join("vendor/patches/libc/src");
    [
        format!("unix/linux_like/linux/musl/b64/{arch}/mod.rs"),
        "unix/linux_like/linux/musl/b64/mod.rs".into(),
        "unix/linux_like/linux/musl/mod.rs".into(),
        "unix/linux_like/linux/arch/generic/mod.rs".into(),
        "unix/linux_like/linux/mod.rs".into(),
        "unix/linux_like/linux_l4re_shared.rs".into(),
        "unix/linux_like/mod.rs".into(),
        "unix/mod.rs".into(),
        // the libc crate's "new structure": FUTEX_* live here
        "new/linux_uapi/linux/futex.rs".into(),
    ]
    .into_iter()
    .map(|p| src.join(p))
    .collect()
}

pub(super) fn scrape_const_names(root: &Path) -> Result<Vec<String>> {
    let src = root.join("vendor/patches/libc/src");
    let mut files: Vec<PathBuf> = ARCHES
        .iter()
        .flat_map(|a| libc_search_paths(root, a))
        .collect();
    for sub in ["new/linux_uapi", "new/musl", "new/common"] {
        util::walk(&src.join(sub), "rs", &mut files)?;
    }
    let mut names = BTreeSet::new();
    for f in files {
        let Ok(text) = fs::read_to_string(&f) else {
            continue;
        };
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("pub const ") else {
                continue;
            };
            let Some((name, _)) = rest.split_once(':') else {
                continue;
            };
            let name = name.trim();
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                names.insert(name.to_string());
            }
        }
    }
    Ok(names.into_iter().collect())
}

/// Build a crate of `static AUDIT_<name>: i128 = libc::<name> as i128` for the
/// real target spec and read the values back out of the LLVM IR. Names that
/// don't exist for this target, or aren't integers, fall out through rustc's
/// error list.
pub(super) fn rust_values(
    root: &Path,
    work: &Path,
    arch: &str,
    names: &[String],
    fns: &[String],
) -> Result<(HashMap<String, i128>, HashSet<String>)> {
    let dir = work.join("rs");
    fs::create_dir_all(dir.join("src"))?;
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"audit-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
             [dependencies]\nlibc = {{ path = \"{}\", default-features = false }}\n\n[workspace]\n",
            root.join("vendor/patches/libc").display()
        ),
    )?;
    const HEADER: &str =
        "#![no_std]\n#![allow(deprecated, overflowing_literals, unused_imports)]\n";
    let header_lines = HEADER.lines().count();
    let target = root.join(format!("generated/{arch}-unknown-linux-musl.json"));
    // one probe per line, constants first, then functions: naming a function
    // only compiles when the crate declares it for this target
    let mut live: Vec<(bool, &String)> = names
        .iter()
        .map(|n| (false, n))
        .chain(fns.iter().map(|n| (true, n)))
        .collect();
    for _round in 0..8 {
        let mut text = String::from(HEADER);
        for (is_fn, n) in &live {
            if *is_fn {
                let _ = writeln!(text, "pub fn audit_fn_{n}() {{ let _ = libc::{n}; }}");
            } else {
                let _ = writeln!(
                    text,
                    "#[unsafe(no_mangle)] pub static AUDIT_{n}: i128 = libc::{n} as i128;"
                );
            }
        }
        fs::write(dir.join("src/lib.rs"), text)?;
        let deps = dir.join(format!("target/{arch}-unknown-linux-musl/debug/deps"));
        if let Ok(rd) = fs::read_dir(&deps) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "ll") {
                    let _ = fs::remove_file(p);
                }
            }
        }
        let out = Command::new("cargo")
            .current_dir(&dir)
            .env("CARGO_INCREMENTAL", "0")
            .args([
                "rustc",
                "--lib",
                "--quiet",
                "--message-format=short",
                "--target",
            ])
            .arg(&target)
            .args([
                "-Zbuild-std=core",
                "-Zjson-target-spec",
                "--",
                "--emit=llvm-ir",
            ])
            .output()
            .context("running cargo for the constant probe")?;
        if out.status.success() {
            let ll = fs::read_dir(&deps)?
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.extension().is_some_and(|x| x == "ll")
                        && p.file_name()
                            .is_some_and(|f| f.to_string_lossy().starts_with("audit_probe"))
                })
                .context("the probe built but left no .ll file")?;
            let exist = live
                .iter()
                .filter(|(is_fn, _)| *is_fn)
                .map(|(_, n)| (*n).clone())
                .collect();
            return Ok((parse_llvm_ir(&fs::read_to_string(ll)?)?, exist));
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut bad: HashSet<usize> = HashSet::new();
        for line in stderr.lines() {
            let Some(rest) = line.strip_prefix("src/lib.rs:") else {
                continue;
            };
            if !rest.contains("error") {
                continue;
            }
            if let Some(n) = rest.split(':').next().and_then(|n| n.parse::<usize>().ok()) {
                bad.insert(n);
            }
        }
        if bad.is_empty() {
            bail!("the constant probe failed in a way that names no line:\n{stderr}");
        }
        live = live
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !bad.contains(&(i + 1 + header_lines)))
            .map(|(_, n)| n)
            .collect();
    }
    bail!("the constant probe for {arch} still doesn't build after 8 rounds of pruning")
}

/// `@AUDIT_X = ... constant [16 x i8] c"\04\00..."` or `zeroinitializer`.
fn parse_llvm_ir(ir: &str) -> Result<HashMap<String, i128>> {
    let mut out = HashMap::new();
    for line in ir.lines() {
        let Some(rest) = line.strip_prefix("@AUDIT_") else {
            continue;
        };
        let Some((name, rest)) = rest.split_once(' ') else {
            continue;
        };
        let value = if rest.contains("zeroinitializer") {
            0
        } else if let Some((_, s)) = rest.split_once("c\"") {
            let s = s.split('"').next().unwrap_or("");
            let mut bytes = Vec::with_capacity(16);
            let raw = s.as_bytes();
            let mut i = 0;
            while i < raw.len() {
                if raw[i] == b'\\' && raw.get(i + 1) == Some(&b'\\') {
                    bytes.push(b'\\');
                    i += 2;
                } else if raw[i] == b'\\' && i + 2 < raw.len() {
                    let hex = std::str::from_utf8(&raw[i + 1..i + 3])?;
                    bytes.push(u8::from_str_radix(hex, 16)?);
                    i += 3;
                } else {
                    bytes.push(raw[i]);
                    i += 1;
                }
            }
            if bytes.len() != 16 {
                bail!(
                    "unexpected initializer width for AUDIT_{name}: {} bytes",
                    bytes.len()
                );
            }
            i128::from_le_bytes(bytes.try_into().unwrap())
        } else if let Some((_, v)) = rest.split_once("constant i128 ") {
            v.split(|c: char| c == ',' || c == ' ')
                .next()
                .unwrap_or("")
                .parse()?
        } else {
            bail!("can't read the initializer of AUDIT_{name}: {line}");
        };
        out.insert(name.to_string(), value);
    }
    Ok(out)
}

// --------------------------------------------------------------------- cosmo
