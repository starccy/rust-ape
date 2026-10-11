use std::sync::OnceLock;

use anyhow::{Context, Result, bail};

use crate::util;
use crate::util::concat_strs;

/// `[toolchain] channel` from rust-toolchain.toml.
pub fn rust_channel() -> Result<&'static str> {
    static CHANNEL: OnceLock<String> = OnceLock::new();
    if let Some(c) = CHANNEL.get() {
        return Ok(c);
    }
    let path = util::repo_root().join("rust-toolchain.toml");
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let table: toml::Table = text
        .parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))?;
    let channel = table
        .get("toolchain")
        .and_then(|t| t.get("channel"))
        .and_then(|c| c.as_str())
        .with_context(|| format!("{} has no [toolchain] channel", path.display()))?;
    if parse_nightly_date(channel).is_none() {
        bail!(
            "channel in {} is {channel:?}; it has to be pinned to a date, like nightly-2026-07-28",
            path.display()
        );
    }
    Ok(CHANNEL.get_or_init(|| channel.to_string()))
}

/// "nightly-2026-07-28" -> "2026-07-28", None if it isn't that shape.
fn parse_nightly_date(channel: &str) -> Option<&str> {
    let date = channel.strip_prefix("nightly-")?;
    let ok = date.len() == 10
        && date.as_bytes()[4] == b'-'
        && date.as_bytes()[7] == b'-'
        && date.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    ok.then_some(date)
}

/// Crates needing a patch, as (name, version); patches live in patches/<name>.patch.
/// libc must match what std depends on in library/Cargo.lock
pub const PATCHED_CRATES: &[(&str, &str)] = &[("libc", "0.2.189")];

/// Pinned to the official release zip. cosmocc itself is unmodified
pub const COSMOCC_VERSION: &str = "4.0.2";
pub const COSMOCC_SHA256: &str = "85b8c37a406d862e656ad4ec14be9f6ce474c1b436b9615e91a55208aced3f44";
pub const COSMOCC_BASE_URL: &str = "https://cosmo.zip/pub/cosmocc/";

/// (target triple, cosmocc's arch prefix). Templates live in targets/<triple>.json
/// and setup renders them into generated/.
pub const TARGETS: &[(&str, &str)] = &[
    ("x86_64-unknown-linux-musl", "x86_64"),
    ("aarch64-unknown-linux-musl", "aarch64"),
];

/// The pinned channel's rust-src, which build-std compiles std from; the
/// component is added if the toolchain lacks it.
pub fn rust_src_dir() -> Result<std::path::PathBuf> {
    let channel = rust_channel()?;
    let sysroot = util::capture(
        std::process::Command::new("rustc")
            .arg(format!("+{channel}"))
            .args(["--print", "sysroot"]),
    )?;
    let src = std::path::Path::new(sysroot.trim()).join("lib/rustlib/src/rust/library");
    if !src.is_dir() {
        util::run(std::process::Command::new("rustup").args([
            "component",
            "add",
            "rust-src",
            "--toolchain",
            channel,
        ]))
        .context("rust-src is needed to build std")?;
    }
    if !src.is_dir() {
        bail!(
            "no rust-src at {} after adding the component",
            src.display()
        );
    }
    Ok(src)
}

/// The libc version std's own Cargo.lock pins.
pub fn std_libc_version() -> Result<String> {
    let lock = rust_src_dir()?.join("Cargo.lock");
    let text = std::fs::read_to_string(&lock)
        .with_context(|| format!("could not read {}", lock.display()))?;
    let table: toml::Table = text
        .parse()
        .with_context(|| format!("{} is not valid TOML", lock.display()))?;
    table
        .get("package")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("libc"))
        .and_then(|p| p.get("version").and_then(|v| v.as_str()))
        .map(str::to_string)
        .with_context(|| format!("{} has no libc entry", lock.display()))
}

/// The patched libc has to be the version std asks for.
pub fn check_std_libc() -> Result<()> {
    let want = std_libc_version()?;
    let (_, have) = PATCHED_CRATES
        .iter()
        .find(|&&(n, _)| n == "libc")
        .context("libc missing from PATCHED_CRATES")?;
    if *have != want {
        bail!(
            "std of {} depends on libc {want}, but patches/libc.patch is for libc {have}.\n\
             Rebase the patch onto {want} and update PATCHED_CRATES in xtask/src/toolchain/config.rs",
            rust_channel()?
        );
    }
    Ok(())
}

pub fn crate_url(name: &str, version: &str) -> String {
    format!("https://static.crates.io/crates/{name}/{name}-{version}.crate")
}

pub fn cosmocc_url() -> &'static str {
    concat_strs!(COSMOCC_BASE_URL, "cosmocc-", COSMOCC_VERSION, ".zip")
}
