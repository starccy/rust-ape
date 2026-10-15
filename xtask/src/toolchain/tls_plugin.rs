//! The LLVM pass plugin behind `thread_local!`, see tls-plugin/plugin.cpp.
//! It has to be built against the headers of the LLVM inside the pinned
//! rustc, so setup downloads the copy .github/workflows/tls-plugin.yml
//! publishes for this channel and source, or builds one itself from the
//! rust-dev artifact of that rustc. Without it the targets stay on LLVM's
//! emulated TLS.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::toolchain::config;
use crate::util;

/// Hash of tls-plugin/, from build.rs
pub const HASH: &str = env!("TLS_PLUGIN_HASH");

const RELEASES: &str = "https://github.com/starccy/rust-ape/releases/download";
const RUST_DEV: &str = "https://ci-artifacts.rust-lang.org/rustc-builds";

pub fn path(root: &Path) -> PathBuf {
    root.join("vendor/tls-plugin/ape-tls.so")
}

/// The release tag and asset name a prebuilt plugin is published under
pub fn release_names() -> Result<(String, String)> {
    let tag = format!("tls-plugin-{}-{HASH}", config::rust_channel()?);
    let asset = format!("ape-tls-{}-linux.so", std::env::consts::ARCH);
    Ok((tag, asset))
}

/// vendor/tls-plugin/ape-tls.so for this channel and source. A plugin that
/// can't be had leaves a warning and no file, unless `build` asked for it.
pub fn fetch(root: &Path, force: bool, build: bool) -> Result<()> {
    let dir = root.join("vendor/tls-plugin");
    let stamp_file = root.join("vendor/.stamps/tls-plugin");
    let stamp = format!("{} {HASH}", config::rust_channel()?);
    if !force && !build && util::up_to_date(&dir, &stamp_file, &stamp) && path(root).is_file() {
        println!("==> tls plugin up to date, skipping");
        return Ok(());
    }
    util::rm_rf(&dir)?;
    fs::create_dir_all(&dir)?;
    let got = if build {
        build_here(root, &dir)
    } else {
        download(root).or_else(|e| {
            println!("==> no prebuilt tls plugin ({e:#}), building one");
            build_here(root, &dir)
        })
    }
    .and_then(|()| smoke_test(root, &dir));
    match got {
        Ok(()) => util::write_stamp(&stamp_file, &stamp),
        Err(e) if !build => {
            util::rm_rf(&dir)?;
            eprintln!(
                "warning: no tls plugin, so thread_local! stays on LLVM's emulated TLS, \
                 which is slower: {e:#}"
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn download(root: &Path) -> Result<()> {
    let (tag, asset) = release_names()?;
    let cached = root.join("cache").join(format!("{tag}-{asset}"));
    util::download(&format!("{RELEASES}/{tag}/{asset}"), &cached)?;
    fs::copy(&cached, path(root))?;
    Ok(())
}

fn build_here(root: &Path, dir: &Path) -> Result<()> {
    let channel = config::rust_channel()?;
    let vv = util::capture(Command::new("rustc").arg(format!("+{channel}")).arg("-vV"))?;
    let field = |k: &str| {
        vv.lines()
            .find_map(|l| l.strip_prefix(k))
            .map(str::trim)
            .with_context(|| format!("rustc -vV has no {k}"))
    };
    let (commit, host) = (field("commit-hash:")?, field("host:")?);
    let tarball = root
        .join("cache")
        .join(format!("rust-dev-{commit}-{host}.tar.xz"));
    util::download(
        &format!("{RUST_DEV}/{commit}/rust-dev-nightly-{host}.tar.xz"),
        &tarball,
    )
    .context("the rust-dev artifact of this rustc carries the LLVM headers")?;
    let headers = dir.join("llvm");
    fs::create_dir_all(&headers)?;
    util::run(
        Command::new("tar")
            .arg("-xJf")
            .arg(&tarball)
            .arg("-C")
            .arg(&headers)
            .args([
                "--strip-components=2",
                "--wildcards",
                "*/rust-dev/include/*",
            ]),
    )?;
    let cxx = std::env::var("CXX")
        .ok()
        .or_else(|| {
            ["c++", "clang++", "g++"]
                .into_iter()
                .find(|c| Command::new(c).arg("--version").output().is_ok())
                .map(String::from)
        })
        .context("building the tls plugin needs a C++ compiler (c++, clang++, g++ or $CXX)")?;
    println!("==> building the tls plugin with {cxx}");
    let built = util::run(
        Command::new(&cxx)
            .args(["-std=c++17", "-fPIC", "-shared", "-O2", "-fno-rtti", "-I"])
            .arg(headers.join("include"))
            .arg(root.join("xtask/tls-plugin/plugin.cpp"))
            .arg("-o")
            .arg(path(root)),
    );
    util::rm_rf(&headers)?;
    built
}

// loads the plugin into the pinned rustc and checks both targets' output
fn smoke_test(root: &Path, dir: &Path) -> Result<()> {
    let src = dir.join("smoke.rs");
    fs::write(
        &src,
        "#![feature(no_core, lang_items, thread_local)]\n#![no_core]\n#![allow(internal_features)]\n\
         #[lang = \"pointee_sized\"]\ntrait PointeeSized {}\n\
         #[lang = \"meta_sized\"]\ntrait MetaSized: PointeeSized {}\n\
         #[lang = \"sized\"]\ntrait Sized: MetaSized {}\n\
         #[lang = \"drop_glue\"]\nfn drop_glue<T: ?Sized>(_: *mut T) {}\n\
         #[thread_local]\nstatic mut V: u8 = 0;\n\
         #[unsafe(no_mangle)]\npub fn v() -> *mut u8 {\n    &raw mut V\n}\n",
    )?;
    let plugin = path(root);
    for (arch, want) in [("x86_64", "__get_tls_rax"), ("aarch64", "__get_tls_x0")] {
        let out = dir.join(format!("smoke-{arch}.s"));
        let res = Command::new("rustc")
            .arg(format!("+{}", config::rust_channel()?))
            .arg(format!("-Zllvm-plugins={}", plugin.display()))
            .arg(format!("--target={arch}-unknown-linux-musl"))
            .args(["--crate-type=lib", "--emit=asm", "-O", "-o"])
            .arg(&out)
            .arg(&src)
            .output()
            .context("could not run rustc")?;
        if !res.status.success() {
            bail!(
                "rustc with the plugin failed ({}): {}",
                res.status,
                String::from_utf8_lossy(&res.stderr).trim()
            );
        }
        let asm = fs::read_to_string(&out)?;
        if !asm.contains(want) {
            bail!("the plugin left the {arch} thread-local access alone");
        }
        fs::remove_file(&out)?;
    }
    fs::remove_file(&src)?;
    Ok(())
}

/// The rustc flags for target units: the plugin when the generated target
/// JSONs ask for real TLS, nothing when they're on emulated TLS
pub fn rustflags(root: &Path, target_json: &Path) -> Result<String> {
    let json = fs::read_to_string(target_json)
        .with_context(|| format!("could not read {}", target_json.display()))?;
    if !json.contains("\"tls-model\": \"local-exec\"") {
        return Ok(String::new());
    }
    let plugin = path(root);
    if !plugin.is_file() {
        bail!(
            "{} wants the tls plugin, but {} is missing. Run `cargo xtask setup`",
            target_json.display(),
            plugin.display()
        );
    }
    let p = plugin.display().to_string();
    if p.contains(char::is_whitespace) {
        bail!("the tls plugin's path has whitespace, which RUSTFLAGS can't carry: {p}");
    }
    // the hash makes cargo rebuild everything when the plugin changes
    Ok(format!(" -Z llvm-plugins={p} --cfg rust_ape_tls_{HASH}"))
}
