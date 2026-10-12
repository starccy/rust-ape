//! `cargo xtask hostlink`, which the linker wrapper runs for every `-l`
//! the link asks for dynamically and cosmo doesn't have. It finds the
//! library among the build machine's shared objects and writes an archive
//! of stubs, one per function the library exports. The linker takes the
//! stubs it needs from there like it would from the library itself. Each
//! one jumps through a slot that libc's `__hostlink_bind()` fills in the
//! first time it's called, loading the library on whatever host the
//! binary runs on.

mod exports;
mod find;
mod sdk;
mod stubs;

pub use self::exports::exports_of;
pub use self::find::{find_library, host_library, lib_symbol};
pub use self::sdk::{framework_exports, sdk_exports};

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use self::stubs::stub_archive;
use crate::metadata::Metadata;

// a hash of this module's source, so cached archives get remade when it changes
pub(super) const LINKGEN_HASH: &str = env!("LINKGEN_HASH");

/// What the command was asked to do; cli/hostlink.rs has the flags.
pub struct Options {
    /// x86_64 or aarch64
    pub arch: String,
    /// cosmocc's C compiler for that arch
    pub cc: PathBuf,
    /// and its ar, runnable as is
    pub ar: PathBuf,
    /// Where the archives go
    pub out_dir: PathBuf,
    /// Library search directories from the link line
    pub search: Vec<PathBuf>,
    /// Library names, as in -l<name>
    pub names: Vec<String>,
}

/// Prints `<name>\t<archive>` for each library it could make stubs for.
/// A library it can't find is left out, so the link reports it as before.
pub fn run(args: &Options) -> Result<()> {
    fs::create_dir_all(&args.out_dir)?;
    for name in &args.names {
        let Some(path) = find_library(name, &args.search)? else {
            continue;
        };
        let archive = stub_archive(args, name, &path)
            .with_context(|| format!("making stubs for -l{name} ({})", path.display()))?;
        println!("{name}\t{}", archive.display());
    }
    Ok(())
}

pub(super) const OSES: [&str; 3] = ["linux", "macos", "windows"];

/// What a project's Cargo.toml says under
/// `[package.metadata.rust-ape.host-libs]`, as JSON for the linker wrapper
/// to hand back to `xtask hostlink`. Each key is a name as in -l<name>,
/// each value may list the file names to try on linux, macos or windows:
///
/// ```toml
/// [package.metadata.rust-ape.host-libs]
/// sqlite3 = { windows = ["winsqlite3.dll", "sqlite3.dll"] }
/// ```
///
/// An empty list says the library has no version on that host: a call
/// into it there ends the program as a missing library does, and the
/// rustc wrapper takes the crate's structs and functions to be for the
/// other hosts (see host/rustcwrap/types.rs). An entry named `*` is the
/// default for every library without one of its own. `sync = false`
/// stops the library's variables being synced around every call on
/// macOS and Windows, leaving the one sync when it loads.
pub fn names_from_metadata(meta: &Metadata) -> Result<String> {
    let mut all = serde_json::Map::new();
    for pkg in meta
        .packages
        .iter()
        .filter(|p| meta.workspace_members.contains(&p.id))
    {
        let Some(libs) = pkg
            .metadata
            .as_ref()
            .and_then(|m| m.pointer("/rust-ape/host-libs"))
        else {
            continue;
        };
        let libs = libs
            .as_object()
            .with_context(|| format!("{}: rust-ape.host-libs must be a table", pkg.name))?;
        for (lib, v) in libs {
            let table = v.as_object().with_context(|| {
                format!("{}: rust-ape.host-libs.{lib} must be a table", pkg.name)
            })?;
            for (os, names) in table {
                if os == "sync" {
                    if !names.is_boolean() {
                        bail!(
                            "{}: rust-ape.host-libs.{lib}.sync must be true or false",
                            pkg.name
                        );
                    }
                    continue;
                }
                if !OSES.contains(&os.as_str()) {
                    bail!(
                        "{}: rust-ape.host-libs.{lib}: {os} isn't sync or one of {OSES:?}",
                        pkg.name
                    );
                }
                let ok = names
                    .as_array()
                    .is_some_and(|a| a.iter().all(|n| n.is_string()));
                if !ok {
                    bail!(
                        "{}: rust-ape.host-libs.{lib}.{os} must be a list of file names",
                        pkg.name
                    );
                }
            }
            all.insert(lib.clone(), v.clone());
        }
    }
    Ok(serde_json::Value::Object(all).to_string())
}

/// What a project's Cargo.toml says under `[package.metadata.rust-ape]`
/// about crates to compile as code for one host, as JSON for the rustc
/// wrapper: `windows-crates = ["foo"]` and `apple-crates = ["bar"]`, see
/// the module comment of host/rustcwrap/mod.rs
pub fn crate_modes_from_metadata(meta: &Metadata) -> Result<String> {
    let mut all = serde_json::Map::new();
    for pkg in meta
        .packages
        .iter()
        .filter(|p| meta.workspace_members.contains(&p.id))
    {
        let Some(m) = pkg.metadata.as_ref().and_then(|m| m.pointer("/rust-ape")) else {
            continue;
        };
        for (key, out) in [("windows-crates", "windows"), ("apple-crates", "apple")] {
            let Some(v) = m.get(key) else { continue };
            let ok = v
                .as_array()
                .is_some_and(|a| a.iter().all(|n| n.is_string()));
            if !ok {
                bail!("{}: rust-ape.{key} must be a list of crate names", pkg.name);
            }
            let list = all
                .entry(out)
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            list.as_array_mut()
                .unwrap()
                .extend(v.as_array().unwrap().iter().cloned());
        }
    }
    Ok(serde_json::Value::Object(all).to_string())
}

/// The file names `host-libs` gives for library `name` on `os`, or the
/// `*` entry's, or none
pub fn host_names(config: &serde_json::Value, name: &str, os: &str) -> Option<Vec<String>> {
    let list = config
        .pointer(&format!("/{name}/{os}"))
        .or_else(|| config.pointer(&format!("/*/{os}")))?
        .as_array()?;
    Some(
        list.iter()
            .filter_map(|n| n.as_str().map(String::from))
            .collect(),
    )
}
