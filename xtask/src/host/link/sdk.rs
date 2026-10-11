//! What a macOS SDK's .tbd stubs say a library or framework exports.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::exports::{Exports, exports_of};

/// What the SDK's stub for `-l<name>` under usr/lib exports, if there is
/// one: the macOS side of a library the build machine may also have as
/// a shared object of its own, like libobjc.
pub fn sdk_exports(name: &str) -> Result<Option<Exports>> {
    let Some(sdk) = macos_sdk() else {
        return Ok(None);
    };
    let p = sdk.join(format!("usr/lib/lib{name}.tbd"));
    if !p.is_file() {
        return Ok(None);
    }
    Ok(Some(exports_of(&p)?))
}

/// The macOS SDK `RUST_APE_MACOS_SDK` (or `SDKROOT`) points at, if any.
pub fn macos_sdk() -> Option<PathBuf> {
    std::env::var_os("RUST_APE_MACOS_SDK")
        .or_else(|| std::env::var_os("SDKROOT"))
        .map(PathBuf::from)
}

pub(super) fn is_tbd(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "tbd")
}

/// The functions a macOS framework exports, read from the stub library in
/// the SDK that `RUST_APE_MACOS_SDK` (or `SDKROOT`) points at. None
/// without an SDK or a stub for that framework. Symbols a framework
/// re-exports from another count as its own.
pub fn framework_exports(name: &str) -> Result<Option<Vec<String>>> {
    let Some(sdk) = macos_sdk() else {
        return Ok(None);
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    if !tbd_symbols(
        &sdk,
        &format!("/System/Library/Frameworks/{name}.framework/{name}"),
        &mut out,
        &mut seen,
    )? {
        return Ok(None);
    }
    out.sort();
    out.dedup();
    Ok(Some(out))
}

// `install` is the library's install name, which is also where its .tbd
// sits under the SDK; false when there is none
fn tbd_symbols(
    sdk: &Path,
    install: &str,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Result<bool> {
    if !seen.insert(install.to_string()) {
        return Ok(true);
    }
    let rel = install.trim_start_matches('/');
    let mut path = sdk.join(format!("{rel}.tbd"));
    if !path.is_file() {
        // Foo.framework/Foo may be a link to Versions/A/Foo
        let p = Path::new(rel);
        let (dir, file) = (
            p.parent().unwrap_or(Path::new("")),
            p.file_name().unwrap_or_default().to_string_lossy(),
        );
        path = sdk.join(dir).join("Versions/A").join(format!("{file}.tbd"));
        if !path.is_file() {
            return Ok(false);
        }
    }
    let t = tbd_read(&path)?;
    out.extend(t.symbols);
    for i in &t.libraries {
        tbd_symbols(sdk, i, out, seen)?;
    }
    Ok(true)
}

struct Tbd {
    pub(super) install: Option<String>,
    symbols: Vec<String>,
    /// install names of what it re-exports
    libraries: Vec<String>,
}

// the stub is YAML: `symbols: [ _a, _b, ... ]` lists, possibly over
// several lines, re-exported install names under `libraries:`, and the
// library's own under `install-name:`
fn tbd_read(path: &Path) -> Result<Tbd> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut t = Tbd {
        install: None,
        symbols: Vec::new(),
        libraries: Vec::new(),
    };
    let mut key = String::new();
    let mut list = String::new();
    let mut in_list = false;
    for line in text.lines() {
        let l = line.trim();
        if !in_list {
            let Some((k, rest)) = l.split_once(':') else {
                continue;
            };
            let k = k.trim_start_matches("- ").trim();
            let rest = rest.trim();
            if k == "install-name" && t.install.is_none() {
                t.install = Some(rest.trim_matches('\'').trim_matches('"').to_string());
                continue;
            }
            if !rest.starts_with('[') {
                continue;
            }
            key = k.to_string();
            list.clear();
            in_list = true;
            list.push_str(rest);
        } else {
            list.push(' ');
            list.push_str(l);
        }
        if in_list && list.contains(']') {
            in_list = false;
            let inner = list.trim_start_matches('[').split(']').next().unwrap_or("");
            let items = inner
                .split(',')
                .map(|s| s.trim().trim_matches('\'').trim_matches('"'))
                .filter(|s| !s.is_empty());
            match key.as_str() {
                "symbols" | "weak-symbols" => {
                    for i in items {
                        if let Some(sym) = i.strip_prefix('_')
                            && sym.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                        {
                            t.symbols.push(sym.to_string());
                        }
                    }
                }
                "libraries" => t.libraries.extend(items.map(String::from)),
                _ => {}
            }
        }
    }
    Ok(t)
}

// what a stub under the SDK's usr/lib exports, with its re-exports:
// every symbol as a function, since a stub doesn't tell variables apart
// and gives no sizes
pub(super) fn tbd_exports(path: &Path) -> Result<Exports> {
    let t = tbd_read(path)?;
    let mut functions = t.symbols;
    if let Some(sdk) = macos_sdk() {
        let mut seen = HashSet::new();
        if let Some(i) = &t.install {
            seen.insert(i.clone());
        }
        for i in &t.libraries {
            tbd_symbols(&sdk, i, &mut functions, &mut seen)?;
        }
    }
    functions.sort();
    functions.dedup();
    Ok(Exports {
        soname: t.install,
        functions,
        data: Vec::new(),
    })
}
