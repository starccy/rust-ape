use crate::concat::concat_strs;
use crate::util;
use anyhow::{Context, Result, bail};
use std::sync::OnceLock;

/// `[toolchain] channel` from rust-toolchain.toml.
///
/// The library patch must exactly match the rust toolchain version
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
            if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() }
        });
    ok.then_some(date)
}

/// Crates needing a patch, as (name, version); patches live in patches/<name>.patch.
/// libc must match what std depends on in library/Cargo.lock
pub const PATCHED_CRATES: &[(&str, &str)] = &[
    ("async-io", "2.6.0"),
    ("async-process", "2.5.0"),
    ("errno", "0.3.14"),
    ("libc", "0.2.189"),
];

/// Pinned to the official release zip. cosmocc itself is unmodified
pub const COSMOCC_VERSION: &str = "4.0.2";
pub const COSMOCC_SHA256: &str =
    "85b8c37a406d862e656ad4ec14be9f6ce474c1b436b9615e91a55208aced3f44";
pub const COSMOCC_BASE_URL: &str = "https://cosmo.zip/pub/cosmocc/";

/// (target triple, cosmocc's arch prefix). Templates live in targets/<triple>.json
/// and setup renders them into generated/.
pub const TARGETS: &[(&str, &str)] = &[
    ("x86_64-unknown-linux-musl", "x86_64"),
    ("aarch64-unknown-linux-musl", "aarch64"),
];

/// Fallback when there's no usable rustup: the dated rust-src tarball on dist.
pub fn rust_src_url() -> Result<String> {
    let date = parse_nightly_date(rust_channel()?).expect("channel was checked to be dated");
    Ok(format!(
        "https://static.rust-lang.org/dist/{date}/rust-src-nightly.tar.xz"
    ))
}

pub fn crate_url(name: &str, version: &str) -> String {
    format!("https://static.crates.io/crates/{name}/{name}-{version}.crate")
}

pub fn cosmocc_url() -> &'static str {
    concat_strs!(COSMOCC_BASE_URL, "cosmocc-", COSMOCC_VERSION, ".zip")
}
