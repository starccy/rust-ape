//! What rustc's command line says about the crate being compiled, and
//! whether its Cargo.toml makes it Windows or macOS code.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::values;

pub(super) struct Crate {
    pub(super) name: String,
    /// as given, which cargo makes relative for a workspace member
    pub(super) root_arg: String,
    pub(super) root: PathBuf,
    pub(super) arch: String,
    pub(super) out_dir: PathBuf,
    pub(super) extra: String,
    pub(super) search: Vec<PathBuf>,
    /// `-l` names linked dynamically, in order
    pub(super) dylibs: Vec<String>,
    /// `-l framework=` names, which rustc won't take for this target
    pub(super) frameworks: Vec<String>,
    /// the cargo features on, for skipping modules that are off
    pub(super) features: HashSet<String>,
    /// Windows code, see the module comment
    pub(super) windows: bool,
    /// macOS code likewise: its `target_os = "macos"` holds
    pub(super) apple: bool,
}

// A crate compiled as code for one host: its own Cargo.toml says
// `windows-crate = true` or `apple-crate = true` under
// [package.metadata.rust-ape], or the project's lists it under
// `windows-crates` / `apple-crates` (RUST_APE_CRATE_MODES, from `cargo
// xtask build`). The windows-* crates are Windows code by name.
pub(super) fn crate_mode(name: &str, mode: &str) -> bool {
    if mode == "windows" && (name == "windows" || name.starts_with("windows_")) {
        return true;
    }
    // the lists name packages, whose library may be called otherwise
    let pkg = std::env::var("CARGO_PKG_NAME")
        .unwrap_or_default()
        .replace('-', "_");
    if let Ok(modes) = std::env::var("RUST_APE_CRATE_MODES")
        && let Ok(modes) = serde_json::from_str::<serde_json::Value>(&modes)
        && let Some(list) = modes.get(mode).and_then(|v| v.as_array())
        && list.iter().any(|n| {
            n.as_str()
                .is_some_and(|n| n.replace('-', "_") == name || n.replace('-', "_") == pkg)
        })
    {
        return true;
    }
    let Some(manifest) = std::env::var_os("CARGO_MANIFEST_DIR") else {
        return false;
    };
    let Ok(text) = fs::read_to_string(Path::new(&manifest).join("Cargo.toml")) else {
        return false;
    };
    let Ok(doc) = text.parse::<toml::Table>() else {
        return false;
    };
    doc.get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("rust-ape"))
        .and_then(|r| r.get(format!("{mode}-crate").as_str()))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub(super) fn parse_args(args: &[String]) -> Option<Crate> {
    // target units only; build scripts and proc macros run on the build machine
    let target = values(args, "--target").into_iter().next()?;
    if !target.ends_with(".json") || !target.contains("/generated/") {
        return None;
    }
    let file = Path::new(target).file_name()?.to_str()?;
    let arch = file.split('-').next()?.to_string();
    let name = values(args, "--crate-name").into_iter().next()?.to_string();
    let root_arg = args[1..]
        .iter()
        .find(|a| a.ends_with(".rs") && Path::new(a).is_file())?;
    let root = std::env::current_dir().ok()?.join(root_arg);
    let out_dir = PathBuf::from(values(args, "--out-dir").into_iter().next()?);
    let extra = args
        .windows(2)
        .find_map(|w| {
            (w[0] == "-C")
                .then(|| w[1].strip_prefix("extra-filename="))
                .flatten()
        })
        .unwrap_or("")
        .to_string();
    let mut search = Vec::new();
    for v in values(args, "-L") {
        let (kind, path) = v.split_once('=').unwrap_or(("all", v));
        if matches!(kind, "native" | "all") {
            search.push(PathBuf::from(path));
        }
    }
    let mut dylibs = Vec::new();
    let mut frameworks = Vec::new();
    for v in values(args, "-l") {
        let (kind, name) = v.split_once('=').unwrap_or(("dylib", v));
        let kind = kind.split(':').next().unwrap_or(kind);
        let name = name.split(':').next().unwrap_or(name);
        if kind == "dylib" && !dylibs.iter().any(|d| d == name) {
            dylibs.push(name.to_string());
        }
        if kind == "framework" && !frameworks.iter().any(|d| d == name) {
            frameworks.push(name.to_string());
        }
    }
    let features = values(args, "--cfg")
        .into_iter()
        .filter_map(|c| {
            c.strip_prefix("feature=\"")
                .and_then(|f| f.strip_suffix('"'))
        })
        .map(String::from)
        .collect();
    let windows = crate_mode(&name, "windows");
    let apple = crate_mode(&name, "apple");
    Some(Crate {
        name,
        root_arg: root_arg.clone(),
        root,
        arch,
        out_dir,
        extra,
        search,
        dylibs,
        frameworks,
        features,
        windows,
        apple,
    })
}

// whether a `#[cfg(feature = "x")]` on an item is on; anything else
// counts as on, since the item is only skipped to save work
pub(super) fn feature_on(attrs: &[syn::Attribute], features: &HashSet<String>) -> bool {
    for a in attrs {
        if !a.path().is_ident("cfg") {
            continue;
        }
        let Ok(nv) = a.parse_args::<syn::MetaNameValue>() else {
            continue;
        };
        if !nv.path.is_ident("feature") {
            continue;
        }
        if let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) = &nv.value
            && !features.contains(&s.value())
        {
            return false;
        }
    }
    true
}

// a crate with no -l of its own can still name a library in #[link]
pub(super) fn mentions_link_attr(dir: &Path) -> bool {
    fn walk(dir: &Path, budget: &mut usize) -> bool {
        let Ok(rd) = fs::read_dir(dir) else {
            return false;
        };
        for e in rd.flatten() {
            if *budget == 0 {
                return false;
            }
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                let n = e.file_name();
                if n != "target" && n != ".git" && walk(&p, budget) {
                    return true;
                }
            } else if p.extension().is_some_and(|x| x == "rs") {
                *budget -= 1;
                if fs::read_to_string(&p).is_ok_and(|t| t.contains("link(name")) {
                    return true;
                }
            }
        }
        false
    }
    walk(dir, &mut 4000)
}
