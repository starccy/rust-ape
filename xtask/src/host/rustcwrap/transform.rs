//! The rewrite of one crate: finding its module files, working out which
//! library owns each declaration, and planning the replacements.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use syn::spanned::Spanned;

use super::codegen::{jump, register, variadic, wrapper};
use super::crate_info::{feature_on, mentions_link_attr, parse_args};
use super::host_edits::{Mode, host_edits};
use super::plan::{Explicit, Plan, Scope};
use super::runtime::helper;
use super::shadow::compile_copy;
use super::source::{
    Src, attr_str, blank, dir_for_path_attr, eval_path, items_at, link_attr, load, scope_items,
    span_bytes, tokens,
};
use super::types::{Signature, Types};
use crate::host::link;

pub(super) fn transform(args: &[String]) -> Result<Option<i32>> {
    let Some(krate) = parse_args(args) else {
        return Ok(None);
    };
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    let out = std::env::var_os("OUT_DIR").map(PathBuf::from);

    // which of the libraries this crate names only the host has
    let mut names = krate.dylibs.clone();
    // std and friends, built from rust-src, only link cosmo's libc
    if manifest
        .as_deref()
        .is_some_and(|m| m.to_string_lossy().contains("/lib/rustlib/src/"))
    {
        return Ok(None);
    }
    let mut frameworks = krate.frameworks.clone();
    let scan_link_attrs = !krate.windows && manifest.as_deref().is_some_and(mentions_link_attr);
    if names.is_empty()
        && frameworks.is_empty()
        && !scan_link_attrs
        && !krate.windows
        && !krate.apple
    {
        return Ok(None);
    }

    // every module file we can find, whether or not it has declarations
    let mut files: Vec<Src> = vec![load(&krate.root)?];
    let mut seen: HashSet<PathBuf> = HashSet::from([krate.root.clone()]);
    let root_dir = krate.root.parent().unwrap().to_path_buf();
    // (file index, module dir, path of inline modules inside the file,
    // the module's path from the crate root)
    let mut scopes_to_visit: Vec<(usize, PathBuf, Vec<usize>, Vec<String>)> =
        vec![(0, root_dir, vec![], vec![])];
    let mut scopes: Vec<Scope> = Vec::new();
    while let Some((fi, dir, inline, modpath)) = scopes_to_visit.pop() {
        scopes.push(Scope {
            fi,
            inline: inline.clone(),
            modpath: modpath.clone(),
        });
        let items = items_at(&files[fi].file.items, &inline);
        let mut new_files = Vec::new();
        for (ii, item) in items.iter().enumerate() {
            match item {
                syn::Item::Mod(m) => {
                    if !feature_on(&m.attrs, &krate.features) {
                        continue;
                    }
                    let mut child = inline.clone();
                    child.push(ii);
                    let mut child_path = modpath.clone();
                    child_path.push(m.ident.to_string());
                    if m.content.is_some() {
                        scopes_to_visit.push((
                            fi,
                            dir.join(m.ident.to_string()),
                            child,
                            child_path,
                        ));
                        continue;
                    }
                    let candidates = match attr_str(&m.attrs, "path") {
                        Some(p) => vec![
                            dir_for_path_attr(&files[fi].path, &dir, inline.is_empty()).join(p),
                        ],
                        None => {
                            let n = m.ident.to_string();
                            vec![dir.join(format!("{n}.rs")), dir.join(&n).join("mod.rs")]
                        }
                    };
                    if let Some(p) = candidates.into_iter().find(|p| p.is_file()) {
                        let child_dir = if p.file_name().is_some_and(|f| f == "mod.rs")
                            || attr_str(&m.attrs, "path").is_some()
                        {
                            p.parent().unwrap().to_path_buf()
                        } else {
                            dir.join(m.ident.to_string())
                        };
                        new_files.push((p, child_dir, child_path));
                    }
                }
                syn::Item::Macro(m) if m.mac.path.is_ident("include") => {
                    if let Some(p) = eval_path(&m.mac.tokens) {
                        let p = if Path::new(&p).is_absolute() {
                            PathBuf::from(p)
                        } else {
                            files[fi].path.parent().unwrap().join(p)
                        };
                        if p.is_file() {
                            // a `mod x;` among the included items is looked
                            // for next to the included file
                            let d = p.parent().unwrap().to_path_buf();
                            new_files.push((p, d, modpath.clone()));
                        }
                    }
                }
                _ => {}
            }
        }
        for (p, d, mp) in new_files {
            if seen.insert(p.clone()) {
                match load(&p) {
                    Ok(src) => {
                        files.push(src);
                        scopes_to_visit.push((files.len() - 1, d, vec![], mp));
                    }
                    // syntax syn doesn't know yet; that file just keeps its declarations
                    Err(_) => {}
                }
            }
        }
    }

    // (file, block index in scope) -> the frameworks its #[link] names
    let mut block_frameworks: HashMap<(usize, usize), Vec<String>> = HashMap::new();
    if scan_link_attrs {
        for Scope { fi, inline, .. } in &scopes {
            for (ii, item) in items_at(&files[*fi].file.items, inline).iter().enumerate() {
                if let syn::Item::ForeignMod(fm) = item {
                    for a in &fm.attrs {
                        if let Some((n, kind)) = link_attr(a) {
                            if kind == "dylib" && !names.contains(&n) {
                                names.push(n);
                            } else if kind == "framework" {
                                if !frameworks.contains(&n) {
                                    frameworks.push(n.clone());
                                }
                                block_frameworks.entry((*fi, ii)).or_default().push(n);
                            }
                        }
                    }
                }
            }
        }
    }

    // symbol -> library, and variable -> (library, size)
    let mut owner: HashMap<String, String> = HashMap::new();
    let mut vars: HashMap<String, (String, u64)> = HashMap::new();
    for n in &names {
        let Some(path) = link::host_library(n, &krate.search, &krate.arch)? else {
            continue;
        };
        let exports = link::exports_of(&path)?;
        for f in exports.functions {
            owner.entry(f).or_insert_with(|| n.clone());
        }
        for (d, size) in exports.data {
            vars.entry(d).or_insert_with(|| (n.clone(), size));
        }
        // and what macOS's own build of it exports, from the SDK's stub
        if let Some(sdk) = link::sdk_exports(n)? {
            for f in sdk.functions {
                owner.entry(f).or_insert_with(|| n.clone());
            }
        }
    }
    // a framework's symbols come from the SDK's stub library when there
    // is one; without it the crate's first framework takes every
    // function no library owns, and macOS's dlsym finds the ones that
    // really live in a framework it depends on
    let mut explicit: BTreeMap<String, Explicit> = BTreeMap::new();
    let mut fallback = None;
    for fw in &frameworks {
        let key = format!("framework:{fw}");
        match link::framework_exports(fw)? {
            Some(syms) => {
                for f in syms {
                    owner.entry(f).or_insert_with(|| key.clone());
                }
            }
            None => {
                if fallback.is_none() {
                    fallback = Some(key.clone());
                }
            }
        }
        explicit.insert(key, Explicit::framework(fw));
    }
    // libraries with variables, whose calls sync them on macOS and Windows
    let with_vars: HashSet<String> = vars.values().map(|(l, _)| l.clone()).collect();
    if owner.is_empty() && vars.is_empty() && !krate.windows && !krate.apple && fallback.is_none() {
        return Ok(None);
    }

    let mut plan = Plan {
        edits: Vec::new(),
        libs: BTreeMap::new(),
        explicit,
        variadic: false,
        linkage: false,
        os_ext: false,
        callbacks: false,
    };
    // what makes a variadic value's symbol this crate's own
    let tag: String = format!("{}{}", krate.name, krate.extra)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if krate.windows || krate.apple {
        let mode = if krate.windows {
            Mode::Windows
        } else {
            Mode::Apple
        };
        for (fi, src) in files.iter().enumerate() {
            host_edits(fi, src, &krate.arch, mode, &mut plan);
        }
    }

    // Whether one of the crate's libraries can be loaded on Windows, the
    // one host whose C data model and calling convention differ from what
    // the crate was compiled with. A library whose `host-libs` entry says
    // `windows = []` can't be, and then nothing below applies to the crate.
    let on_windows = !krate.windows && {
        let config = std::env::var("RUST_APE_HOST_LIBS")
            .ok()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .unwrap_or(serde_json::Value::Null);
        let listed = |key: &str| link::host_names(&config, key, "windows").map(|a| !a.is_empty());
        names.iter().any(|n| listed(n).unwrap_or(true))
            || frameworks.iter().any(|f| listed(f).unwrap_or(false))
    };
    let types = Types::collect(&files, &scopes, on_windows);
    if on_windows {
        types.rewrite(&scopes, &mut plan);
    }

    for (si, Scope { fi, inline, .. }) in scopes.iter().enumerate() {
        let src = &files[*fi];
        let (items, end) = scope_items(src, inline);
        let mut generated = String::new();
        for (ii, item) in items.iter().enumerate() {
            let syn::Item::ForeignMod(fm) = item else {
                continue;
            };
            let abi = fm.abi.name.as_ref().map_or("C".to_string(), |n| n.value());
            if !matches!(abi.as_str(), "C" | "system" | "C-unwind" | "system-unwind") {
                continue;
            }
            // a framework link attribute goes, since rustc won't take it
            for a in &fm.attrs {
                if link_attr(a).is_some_and(|(_, kind)| kind == "framework") {
                    let r = a.span().byte_range();
                    let (s, e) = (src.base + r.start, src.base + r.end);
                    plan.edits.push((*fi, s, e, blank(&src.text[s..e])));
                }
            }
            let block_fallback = block_frameworks
                .get(&(*fi, ii))
                .and_then(|v| v.first())
                .map(|fw| format!("framework:{fw}"))
                .or_else(|| fallback.clone());
            for fitem in &fm.items {
                if let syn::ForeignItem::Static(st) = fitem {
                    let sym =
                        attr_str(&st.attrs, "link_name").unwrap_or_else(|| st.ident.to_string());
                    if let Some((lib, size)) = vars.get(&sym) {
                        let libsym = link::lib_symbol(lib);
                        generated.push_str(&register(st, &sym, &libsym, *size));
                        plan.libs.insert(lib.clone(), libsym);
                    }
                    continue;
                }
                let syn::ForeignItem::Fn(f) = fitem else {
                    continue;
                };
                let sym =
                    attr_str(&f.attrs, "link_name").unwrap_or_else(|| f.sig.ident.to_string());
                // the C library's wcslen counts 32-bit units here; Windows
                // code means 16-bit ones
                if krate.windows && sym == "wcslen" && f.sig.inputs.len() == 1 {
                    let ty = match f.sig.inputs.first() {
                        Some(syn::FnArg::Typed(t)) => tokens(&t.ty),
                        _ => continue,
                    };
                    let (vis, name) = (tokens(&f.vis), &f.sig.ident);
                    generated.push_str(&format!(
                        "\n#[allow(warnings, clippy::all)]\n{vis} unsafe fn {name}(s: {ty}) -> usize {{ \
let p: *const u16 = crate::__ape_hostlink::__core::mem::transmute_copy(&s); \
let mut n = 0; unsafe {{ while *p.add(n) != 0 {{ n += 1; }} }} n }}\n"
                    ));
                    let (s, e) = span_bytes(src, fitem.span(), &f.attrs);
                    plan.edits.push((*fi, s, e, blank(&src.text[s..e])));
                    continue;
                }
                let Some(lib) = owner.get(&sym).cloned().or_else(|| block_fallback.clone()) else {
                    continue;
                };
                if f.attrs.iter().any(|a| a.path().is_ident("link_ordinal")) {
                    continue;
                }
                // where the library's HostLinkLib is: made by the archive
                // of stubs, or by the helper module for an explicit one
                let libexpr = match plan.explicit.get(&lib) {
                    Some(x) => format!("crate::__ape_hostlink::{}.as_ptr()", x.sym),
                    None => {
                        let libsym = link::lib_symbol(&lib);
                        plan.libs.insert(lib.clone(), libsym.clone());
                        format!("&raw const crate::__ape_hostlink::{libsym}")
                    }
                };
                if f.sig.variadic.is_some() {
                    let Some(code) = variadic(f, &sym, &libexpr, &tag) else {
                        continue;
                    };
                    generated.push_str(&code);
                    plan.variadic = true;
                } else if f.sig.inputs.is_empty()
                    && matches!(f.sig.output, syn::ReturnType::Default)
                {
                    generated.push_str(&jump(f, &abi, &sym, &libexpr));
                } else {
                    let model = if on_windows {
                        types.signature(si, &f.sig)
                    } else {
                        Signature::default()
                    };
                    generated.push_str(&wrapper(
                        f,
                        &abi,
                        &sym,
                        &libexpr,
                        with_vars.contains(&lib),
                        &model,
                    ));
                }
                let (s, e) = span_bytes(src, fitem.span(), &f.attrs);
                plan.edits.push((*fi, s, e, blank(&src.text[s..e])));
            }
        }
        if !generated.is_empty() {
            plan.linkage = true;
            plan.edits.push((*fi, end, end, generated));
        }
    }
    if plan.edits.is_empty() {
        return Ok(None);
    }
    let root_end = files[0].text.len();
    let reexport = plan.callbacks && !types.names.contains("HostCallback");
    plan.edits.push((
        0,
        root_end,
        root_end,
        helper(
            &plan.libs,
            &plan.explicit,
            plan.os_ext,
            plan.callbacks,
            reexport,
        ),
    ));
    let at = files[0].base;
    let mut features = Vec::new();
    if plan.linkage {
        features.push("linkage");
    }
    if plan.variadic {
        features.push("unboxed_closures, fn_traits");
    }
    // the notes put on the crate's own items are for its users; on the
    // first line, so the lines keep their numbers
    let mut inner = "#![allow(deprecated)]".to_string();
    if !features.is_empty() {
        let _ = write!(inner, " #![feature({})]", features.join(", "));
    }
    plan.edits.push((0, at, at, inner + " "));

    // apply
    let mut texts: BTreeMap<usize, String> = BTreeMap::new();
    let mut edits = plan.edits;
    edits.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
    for (fi, s, e, t) in edits {
        let text = texts.entry(fi).or_insert_with(|| files[fi].text.clone());
        text.replace_range(s..e, &t);
    }

    let mut changed: HashMap<PathBuf, String> = HashMap::new();
    for (fi, t) in texts {
        changed.insert(files[fi].path.clone(), t);
    }
    compile_copy(args, &krate, manifest.as_deref(), out.as_deref(), changed)
}
