//! Finding a library's shared object on the build machine.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::sdk::macos_sdk;
use crate::util;

// what cosmo's libc.a covers under glibc's names, or the toolchain itself
const LIBC_PARTS: &[&str] = &[
    "c", "m", "dl", "pthread", "rt", "util", "resolv", "crypt", "anl", "nsl", "mvec", "gcc",
    "gcc_s", "gcc_eh", "atomic", "stdc++", "c++", "c++abi", "unwind",
];

pub fn is_sysdir(dir: &Path) -> bool {
    ["/lib", "/lib64", "/usr/lib", "/usr/lib64", "/usr/local/lib"]
        .iter()
        .any(|d| dir.starts_with(d))
}

/// The shared object behind `-l<name>` when it has to come from the host:
/// cosmo doesn't provide it and no crate built it as a static archive in
/// one of the `search` directories. The same rule the linker wrapper
/// applies to the link line.
pub fn host_library(name: &str, search: &[PathBuf], arch: &str) -> Result<Option<PathBuf>> {
    if LIBC_PARTS.contains(&name) {
        return Ok(None);
    }
    let cosmo =
        util::repo_root().join(format!("vendor/cosmocc/{arch}-linux-cosmo/lib/lib{name}.a"));
    if cosmo.exists() {
        return Ok(None);
    }
    if search
        .iter()
        .any(|d| !is_sysdir(d) && d.join(format!("lib{name}.a")).exists())
    {
        return Ok(None);
    }
    find_library(name, search)
}

/// The symbol an archive of stubs gives the library's HostLinkLib, which
/// the wrappers made from a crate's declarations refer to
pub fn lib_symbol(name: &str) -> String {
    let id: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("__hostlink_lib_{id}")
}

const SYSTEM_DIRS: &[&str] = &[
    "/usr/local/lib",
    "/usr/lib/x86_64-linux-gnu",
    "/lib/x86_64-linux-gnu",
    "/usr/lib/aarch64-linux-gnu",
    "/lib/aarch64-linux-gnu",
    "/usr/lib64",
    "/lib64",
    "/usr/lib",
    "/lib",
];

// the way ld resolves -lname against a shared object, minus the static
// archives, which cosmo couldn't link anyway
pub fn find_library(name: &str, search: &[PathBuf]) -> Result<Option<PathBuf>> {
    let file = format!("lib{name}.so");
    let dirs: Vec<&Path> = search
        .iter()
        .map(PathBuf::as_path)
        .chain(SYSTEM_DIRS.iter().map(Path::new))
        .collect();
    for dir in &dirs {
        let p = dir.join(&file);
        if p.is_file() {
            return resolve_script(&p);
        }
    }
    // without the -dev package there's no libfoo.so, only the runtime's
    // libfoo.so.N and the file it points to; take the one with the
    // shortest version, which is the SONAME
    for dir in &dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_prefix(&format!("{file}.")))
                    .is_some_and(|v| {
                        v.split('.')
                            .all(|x| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit()))
                    })
            })
            .collect();
        found.sort_by_key(|p| (p.as_os_str().len(), p.clone()));
        if let Some(p) = found.first() {
            return resolve_script(p);
        }
    }
    // a library only macOS has (libobjc, say) is its stub in the SDK,
    // which lists what it exports
    if let Some(sdk) = macos_sdk() {
        let p = sdk.join(format!("usr/lib/lib{name}.tbd"));
        if p.is_file() {
            return Ok(Some(p));
        }
    }
    Ok(None)
}

// libfoo.so may be a linker script, like `INPUT(libfoo.so.6 -lbar)`,
// in which case the first shared object it names is the library
fn resolve_script(path: &Path) -> Result<Option<PathBuf>> {
    let data = fs::read(path)?;
    if data.starts_with(b"\x7fELF") {
        return Ok(Some(fs::canonicalize(path)?));
    }
    let text = String::from_utf8_lossy(&data);
    let dir = path.parent().unwrap_or(Path::new("/"));
    for tok in text.split(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        if tok.contains(".so") && !tok.starts_with('-') {
            let p = if tok.starts_with('/') {
                PathBuf::from(tok)
            } else {
                dir.join(tok)
            };
            if p.is_file() && fs::read(&p)?.starts_with(b"\x7fELF") {
                return Ok(Some(fs::canonicalize(p)?));
            }
        }
    }
    Ok(None)
}
