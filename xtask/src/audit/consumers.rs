//! What the built crates import from libc, and which functions carry
//! the constants.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::ARCHES;
use super::libc_crate::libc_search_paths;
use crate::toolchain::config;
use crate::util;

/// Who mentions a constant by name: std, or crates in the project's
/// dependency tree. Only used to rank findings.
pub(super) struct References {
    std: HashSet<String>,
    pub(super) crates: BTreeMap<String, HashSet<String>>,
}

impl References {
    pub(super) fn collect(
        root: &Path,
        project: &Path,
        built: &HashSet<&str>,
    ) -> Result<References> {
        let mut std = HashSet::new();
        let library = config::rust_src_dir()?;
        for sub in [
            "std/src",
            "core/src",
            "alloc/src",
            "panic_unwind/src",
            "panic_abort/src",
        ] {
            let mut files = Vec::new();
            util::walk(&library.join(sub), "rs", &mut files)?;
            for f in files {
                idents(&f, &mut std);
            }
        }
        let mut crates = BTreeMap::new();
        let manifest = root.join(project).join("Cargo.toml");
        let meta = util::capture(
            Command::new("cargo")
                .args(["metadata", "--format-version=1", "--manifest-path"])
                .arg(&manifest),
        )?;
        let meta: serde_json::Value = serde_json::from_str(&meta)?;
        for pkg in meta["packages"].as_array().into_iter().flatten() {
            let name = pkg["name"].as_str().unwrap_or("");
            // crates that carry their own tables (linux-raw-sys, windows-sys)
            // or never made it into this build say nothing about libc's values
            let uses_libc = pkg["dependencies"]
                .as_array()
                .is_some_and(|d| d.iter().any(|d| d["name"] == "libc" && d["kind"].is_null()));
            // the project itself carries the generated table check, which
            // names every constant the shim knows
            let local = pkg["source"].is_null();
            if name == "libc"
                || local
                || !uses_libc
                || !built.contains(name.replace('-', "_").as_str())
            {
                continue;
            }
            let Some(dir) = pkg["manifest_path"]
                .as_str()
                .and_then(|p| Path::new(p).parent())
            else {
                continue;
            };
            let mut files = Vec::new();
            util::walk(&dir.join("src"), "rs", &mut files)?;
            let mut set = HashSet::new();
            for f in files {
                idents(&f, &mut set);
            }
            crates.insert(name.to_string(), set);
        }
        Ok(References { std, crates })
    }

    pub(super) fn users(&self, name: &str) -> String {
        let mut out = Vec::new();
        if self.std.contains(name) {
            out.push("std");
        }
        for (krate, set) in &self.crates {
            if set.contains(name) {
                out.push(krate.as_str());
            }
        }
        out.join(" ")
    }
}

/// Upper-case identifiers of one source file.
fn idents(path: &Path, out: &mut HashSet<String>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let mut cur = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_alphanumeric() || c == '_' {
            cur.push(c);
            continue;
        }
        if cur.len() > 2
            && cur.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
            && !cur.chars().any(|c| c.is_ascii_lowercase())
        {
            out.insert(std::mem::take(&mut cur));
        }
        cur.clear();
    }
}

/// Undefined symbols of every rlib the project built that libcosmo.a
/// defines, with the crates importing each.
pub(super) fn libc_imports(
    root: &Path,
    project: &Path,
    profile: &str,
) -> Result<(BTreeMap<String, BTreeSet<String>>, HashSet<String>)> {
    let deps = root
        .join(project)
        .join(format!("target/x86_64-unknown-linux-musl/{profile}/deps"));
    let mut rlibs = Vec::new();
    util::walk(&deps, "rlib", &mut rlibs)?;
    if rlibs.is_empty() {
        bail!(
            "no rlibs under {}; run `cargo xtask build {}` first",
            deps.display(),
            project.display()
        );
    }
    // cargo leaves older builds of a crate next to the current one
    let mut newest: HashMap<String, (std::time::SystemTime, PathBuf)> = HashMap::new();
    for p in rlibs {
        let mtime = fs::metadata(&p)?.modified()?;
        let krate = crate_of(&p);
        if newest.get(&krate).is_none_or(|(t, _)| *t < mtime) {
            newest.insert(krate, (mtime, p));
        }
    }
    let rlibs: Vec<PathBuf> = newest.into_values().map(|(_, p)| p).collect();

    let nm = root.join("vendor/cosmocc/bin/x86_64-linux-cosmo-nm");
    let defined: HashSet<String> = nm_lines(
        util::ape_command(&nm)
            .args(["--defined-only", "-g"])
            .arg(root.join("vendor/cosmocc/x86_64-linux-cosmo/lib/libcosmo.a")),
    )?
    .iter()
    .filter_map(|l| l.split_whitespace().last().map(str::to_string))
    .collect();

    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for chunk in rlibs.chunks(200) {
        for line in nm_lines(util::ape_command(&nm).args(["-u", "-A"]).args(chunk))? {
            // path/libfoo-hash.rlib:member.o:                 U symbol
            let Some(sym) = line.split_whitespace().last() else {
                continue;
            };
            if !defined.contains(sym) && !sym.starts_with("__ape_shim_") {
                continue;
            }
            let file = line.split(".rlib:").next().unwrap_or("");
            out.entry(sym.to_string())
                .or_default()
                .insert(crate_of(Path::new(file)));
        }
    }
    Ok((out, defined))
}

/// The libc functions std itself calls, against the committed list. The
/// shim's two remaining jobs (the sigaction repack, the .exe retry on
/// stat/access/exec) only work if std keeps calling the entry points
/// they wrap, and a toolchain bump can move std onto others (statx for
/// stat, say) without anything failing to build. Additions and removals
/// both need a look; the file is rewritten with --update-std-imports.
pub(super) fn check_std_imports(
    root: &Path,
    imports: &BTreeMap<String, BTreeSet<String>>,
    update: bool,
) -> Result<bool> {
    let file = root.join("patches/std-imports.txt");
    let now: BTreeSet<&str> = imports
        .iter()
        .filter(|(_, crates)| crates.contains("std"))
        .map(|(sym, _)| sym.as_str())
        .collect();
    if update {
        let mut text = String::from(
            "# The libc functions std calls, as found by `cargo xtask audit` in the\n\
             # examples build. Regenerate with `cargo xtask audit --update-std-imports`\n\
             # after checking that every new name lands where it should: a name the\n\
             # shim wraps for policy has to keep its __ape_shim_ redirect, and a new\n\
             # Linux-only call needs cosmo to answer it on every host.\n",
        );
        for s in &now {
            text.push_str(s);
            text.push('\n');
        }
        fs::write(&file, text)?;
        println!("==> wrote {} ({} names)", file.display(), now.len());
        return Ok(true);
    }
    let text = fs::read_to_string(&file).with_context(|| {
        format!(
            "missing {}; run `cargo xtask audit --update-std-imports` once",
            file.display()
        )
    })?;
    let before: BTreeSet<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let added: Vec<&&str> = now.difference(&before).collect();
    let removed: Vec<&&str> = before.difference(&now).collect();
    if added.is_empty() && removed.is_empty() {
        println!("std imports: {} names, unchanged", now.len());
        return Ok(true);
    }
    println!("\nstd's libc imports differ from patches/std-imports.txt:");
    for s in &added {
        println!("  + {s}");
    }
    for s in &removed {
        println!("  - {s}");
    }
    Ok(false)
}

/// `deps/libfoo_bar-0123abcd.rlib` (with or without the extension) -> `foo_bar`
fn crate_of(rlib: &Path) -> String {
    let stem = rlib
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = stem.strip_suffix(".rlib").unwrap_or(&stem);
    let stem = stem.strip_prefix("lib").unwrap_or(stem);
    stem.rsplit_once('-').map_or(stem, |(k, _)| k).to_string()
}

/// nm complains on stderr about each rlib's metadata member and exits
/// nonzero for it; only stdout matters.
fn nm_lines(cmd: &mut Command) -> Result<Vec<String>> {
    let out = cmd.output().context("running nm")?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

pub(super) struct Decl {
    /// (name, type)
    pub(super) params: Vec<(String, String)>,
    /// carries a `link_name = "__ape_shim_..."` under cfg rust_ape_shim
    pub(super) redirected: bool,
}

/// Every `pub fn` of the libc crate's linux-musl files.
pub(super) fn fn_signatures(root: &Path) -> Result<HashMap<String, Decl>> {
    let mut out: HashMap<String, Decl> = HashMap::new();
    let mut files: Vec<PathBuf> = ARCHES
        .iter()
        .flat_map(|a| libc_search_paths(root, a))
        .collect();
    for sub in ["new/linux_uapi", "new/musl", "new/common"] {
        util::walk(
            &root.join("vendor/patches/libc/src").join(sub),
            "rs",
            &mut files,
        )?;
    }
    let files: BTreeSet<PathBuf> = files.into_iter().collect();
    for f in files {
        let Ok(text) = fs::read_to_string(&f) else {
            continue;
        };
        let mut rest = text.as_str();
        while let Some(i) = rest.find("pub fn ") {
            // attributes sit between the end of the previous item and here
            let before = &rest[..i];
            let item_start = before.rfind([';', '{', '}']).map_or(0, |k| k + 1);
            let attrs = &before[item_start..];
            let redirected = attrs.contains("rust_ape_shim") && attrs.contains("__ape_shim_");
            rest = &rest[i + 7..];
            let Some(open) = rest.find('(') else { break };
            let name = rest[..open].trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            // up to the matching paren; fn-pointer params nest
            let mut depth = 0;
            let mut end = None;
            for (j, c) in rest[open..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(open + j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(end) = end else { break };
            let mut params = Vec::new();
            let mut depth = 0;
            let mut cur = String::new();
            for c in rest[open + 1..end].chars().chain(std::iter::once(',')) {
                match c {
                    '(' | '<' | '[' => depth += 1,
                    ')' | '>' | ']' => depth -= 1,
                    _ => {}
                }
                if c == ',' && depth == 0 {
                    if let Some((n, t)) = cur.split_once(':') {
                        params.push((
                            n.trim().to_string(),
                            t.split_whitespace().collect::<Vec<_>>().join(" "),
                        ));
                    }
                    cur.clear();
                } else {
                    cur.push(c);
                }
            }
            // cfg_if! branches for other systems declare the same names; the
            // branch carrying the redirect is the one this target compiles
            match out.entry(name.to_string()) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if redirected && !e.get().redirected {
                        e.insert(Decl { params, redirected });
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(Decl { params, redirected });
                }
            }
        }
    }
    Ok(out)
}

/// Integer parameters that aren't plainly a descriptor, a size or an id,
/// and pointers to structs whose fields hold constants.
pub(super) fn suspect_params(params: &[(String, String)]) -> Vec<String> {
    const INT_TYPES: &[&str] = &[
        "c_int",
        "c_uint",
        "c_long",
        "c_ulong",
        "c_short",
        "c_ushort",
        "i32",
        "u32",
        "i64",
        "u64",
        "Ioctl",
        "clockid_t",
        "idtype_t",
        "nl_item",
        "__rlimit_resource_t",
        "__priority_which_t",
    ];
    const NUMBERS: &[&str] = &[
        "fd",
        "fd1",
        "fd2",
        "fildes",
        "filedes",
        "oldfd",
        "newfd",
        "dirfd",
        "olddirfd",
        "newdirfd",
        "sockfd",
        "socket",
        "sock",
        "s",
        "out_fd",
        "in_fd",
        "epfd",
        "src",
        "dst",
        "duration",
        "len",
        "size",
        "count",
        "n",
        "nbyte",
        "nbytes",
        "num",
        "c",
        "status",
        "code",
        "nfds",
        "timeout",
        "seconds",
        "secs",
        "usec",
        "base",
        "iovcnt",
        "nmemb",
        "maxevents",
        "backlog",
        "uid",
        "gid",
        "pid",
        "pgid",
        "pgrp",
        "sid",
        "owner",
        "group",
        "incr",
        "cpu",
        "errnum",
        "ngroups",
        "setlen",
        "stream",
        "ch",
        "key",
        "value",
        "val",
        "offset",
        "rank",
        "prio",
        "priority",
        "idx",
        "index",
        "i",
        "nochdir",
        "noclose",
        "proj_id",
        "nelem",
        "net",
        "argc",
        "proto",
        "port",
        "loc",
        "suffixlen",
        "stayopen",
        "seed",
        "pshared",
        "shared",
        "errcode",
    ];
    const STRUCTS: &[&str] = &[
        "termios",
        "sigaction",
        "flock",
        "pollfd",
        "sockaddr",
        "msghdr",
        "mmsghdr",
        "addrinfo",
        "epoll_event",
        "sigevent",
        "stack_t",
        "siginfo_t",
        "itimerspec",
        "rlimit",
    ];
    let mut out = Vec::new();
    for (name, ty) in params {
        let bare = ty.rsplit("::").next().unwrap_or(ty);
        let last = ty.split_whitespace().last().unwrap_or("");
        let last = last.rsplit("::").next().unwrap_or(last);
        if INT_TYPES.contains(&bare) && !NUMBERS.contains(&name.as_str()) {
            out.push(format!("{name}: {bare}"));
        } else if ty.starts_with('*') && STRUCTS.contains(&last) {
            out.push(format!("{name}: *{last}"));
        }
    }
    out
}
