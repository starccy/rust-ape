//! The copy of a windows-family proc macro crate whose output gets its
//! `extern "system"` fixed at expansion time.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use syn::spanned::Spanned;

use super::crate_info::Crate;
use super::shadow::compile_copy;
use super::source::{blank, load, tokens};
use super::{host_abi, values};

// A proc macro of the windows family runs on the build machine and
// writes `extern "system"` into whichever crate uses it, which the
// rewrite of that crate's source never sees (`#[interface]` makes COM
// vtables that way). The macro's crate is compiled from a copy in which
// each entry point hands its output through `__ape_abi`, replacing the
// literal after `extern` as RUST_APE_HOST_ABI says at expansion time;
// one compiled macro serves both target architectures.
pub(super) fn windows_macro(args: &[String]) -> Result<Option<i32>> {
    if !values(args, "--crate-type").contains(&"proc-macro") || host_abi(args).is_some() {
        return Ok(None);
    }
    let Some(name) = values(args, "--crate-name").into_iter().next() else {
        return Ok(None);
    };
    if !name.starts_with("windows_") {
        return Ok(None);
    }
    let Some(root_arg) = args[1..]
        .iter()
        .find(|a| a.ends_with(".rs") && Path::new(a).is_file())
    else {
        return Ok(None);
    };
    let root = std::env::current_dir()?.join(root_arg);
    let Some(out_dir) = values(args, "--out-dir").into_iter().next() else {
        return Ok(None);
    };
    let extra = args
        .windows(2)
        .find_map(|w| {
            (w[0] == "-C")
                .then(|| w[1].strip_prefix("extra-filename="))
                .flatten()
        })
        .unwrap_or("")
        .to_string();
    let src = load(&root)?;
    let mut text = src.text.clone();
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut appended = String::new();
    for item in &src.file.items {
        let syn::Item::Fn(f) = item else { continue };
        let Some(attr) = f.attrs.iter().find(|a| {
            a.path().is_ident("proc_macro_attribute")
                || a.path().is_ident("proc_macro")
                || a.path().is_ident("proc_macro_derive")
        }) else {
            continue;
        };
        let mut params = Vec::new();
        for a in &f.sig.inputs {
            match a {
                syn::FnArg::Typed(t) => match &*t.pat {
                    syn::Pat::Ident(p) => params.push(p.ident.to_string()),
                    _ => return Ok(None),
                },
                syn::FnArg::Receiver(_) => return Ok(None),
            }
        }
        let r = attr.span().byte_range();
        edits.push((
            src.base + r.start,
            src.base + r.end,
            blank(&text[src.base + r.start..src.base + r.end]),
        ));
        let r = f.sig.ident.span().byte_range();
        let name = f.sig.ident.to_string();
        edits.push((
            src.base + r.start,
            src.base + r.end,
            format!("__ape_{name}"),
        ));
        // a proc-macro crate exports nothing but the macros
        if !matches!(f.vis, syn::Visibility::Inherited) {
            let r = f.vis.span().byte_range();
            edits.push((
                src.base + r.start,
                src.base + r.end,
                blank(&text[src.base + r.start..src.base + r.end]),
            ));
        }
        let inputs: Vec<String> = f.sig.inputs.iter().map(|a| tokens(a)).collect();
        let _ = write!(
            appended,
            "\n{attr} {vis} fn {name}({inputs}) {ret} {{ crate::__ape_abi(crate::__ape_{name}({args})) }}\n",
            attr = tokens(attr),
            vis = tokens(&f.vis),
            inputs = inputs.join(", "),
            ret = tokens(&f.sig.output),
            args = params.join(", "),
        );
    }
    if appended.is_empty() {
        return Ok(None);
    }
    appended.push_str(ABI_FIX);
    edits.sort_by(|a, b| b.0.cmp(&a.0));
    for (s, e, t) in edits {
        text.replace_range(s..e, &t);
    }
    text.push_str(&appended);
    let krate = Crate {
        name: name.to_string(),
        root_arg: root_arg.clone(),
        root: root.clone(),
        arch: String::new(),
        out_dir: PathBuf::from(out_dir),
        extra,
        search: Vec::new(),
        dylibs: Vec::new(),
        frameworks: Vec::new(),
        features: HashSet::new(),
        windows: true,
        apple: false,
    };
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    let out = std::env::var_os("OUT_DIR").map(PathBuf::from);
    compile_copy(
        args,
        &krate,
        manifest.as_deref(),
        out.as_deref(),
        HashMap::from([(root, text)]),
    )
}

const ABI_FIX: &str = r#"
fn __ape_abi(ts: ::proc_macro::TokenStream) -> ::proc_macro::TokenStream {
    use ::proc_macro::{Group, Literal, TokenStream, TokenTree};
    fn fix(ts: TokenStream, abi: &str) -> TokenStream {
        let mut out: Vec<TokenTree> = Vec::new();
        let mut after_extern = false;
        for t in ts {
            let t = match t {
                TokenTree::Group(g) => {
                    let mut n = Group::new(g.delimiter(), fix(g.stream(), abi));
                    n.set_span(g.span());
                    after_extern = false;
                    TokenTree::Group(n)
                }
                TokenTree::Literal(l) if after_extern => {
                    after_extern = false;
                    let rep = match l.to_string().as_str() {
                        "\"system\"" => Some(abi.to_string()),
                        "\"system-unwind\"" => Some(format!("{abi}-unwind")),
                        _ => None,
                    };
                    match rep {
                        Some(r) => {
                            let mut n = Literal::string(&r);
                            n.set_span(l.span());
                            TokenTree::Literal(n)
                        }
                        None => TokenTree::Literal(l),
                    }
                }
                TokenTree::Ident(i) => {
                    after_extern = i.to_string() == "extern";
                    TokenTree::Ident(i)
                }
                t => {
                    after_extern = false;
                    t
                }
            };
            out.push(t);
        }
        out.into_iter().collect()
    }
    match ::std::env::var("RUST_APE_HOST_ABI") {
        Ok(abi) if abi != "system" => fix(ts, &abi),
        _ => ts,
    }
}
"#;
