//! The edits that make a crate's code the code of one host: ABI strings,
//! cfg predicates and `link!` invocations.

use proc_macro2::TokenStream;

use super::plan::{Explicit, Plan};
use super::source::{Src, tokens};

// what a `link!("x.dll" "system" fn f(..) -> r)` invocation declares
struct LinkDecl {
    pub(super) dll: String,
    pub(super) sym: Option<String>,
    pub(super) sig: syn::Signature,
}

impl syn::parse::Parse for LinkDecl {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let dll: syn::LitStr = input.parse()?;
        let _abi: syn::LitStr = input.parse()?;
        let sym = if input.peek(syn::LitStr) {
            Some(input.parse::<syn::LitStr>()?.value())
        } else {
            None
        };
        let sig: syn::Signature = input.parse()?;
        Ok(LinkDecl {
            dll: dll.value(),
            sym,
            sig,
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Mode {
    Windows,
    Apple,
}

// The edits that make a crate's code the code of one host. For Windows,
// in the x86-64 half, every `extern "system"` becomes `extern "win64"`,
// and every `link!` invocation a function of ours; on both hosts the
// cfg predicates for that host hold. All found in the token stream, so
// macro bodies and invocations count too.
pub(super) fn host_edits(fi: usize, src: &Src, arch: &str, mode: Mode, plan: &mut Plan) {
    use proc_macro2::{Delimiter, TokenTree};
    let Ok(tokens) = src.text[src.base..].parse::<TokenStream>() else {
        return;
    };
    let x86 = arch == "x86_64";
    let mut abis: Vec<(usize, usize, &str)> = Vec::new();
    let mut links: Vec<(usize, usize, TokenStream)> = Vec::new();
    // `windows`, or `target_os = "macos"` and `target_vendor = "apple"`,
    // in a cfg predicate holds: the crate is that host's code on every
    // host, and its parts for that host are the ones that work
    fn cfg_walk(
        ts: TokenStream,
        base: usize,
        mode: Mode,
        cfgs: &mut Vec<(usize, usize, &'static str)>,
    ) {
        let toks: Vec<TokenTree> = ts.into_iter().collect();
        for (i, t) in toks.iter().enumerate() {
            match t {
                TokenTree::Ident(id) if mode == Mode::Windows && id == "windows" => {
                    let assigned =
                        matches!(toks.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == '=');
                    let path =
                        i >= 1 && matches!(&toks[i - 1], TokenTree::Punct(p) if p.as_char() == ':');
                    if !assigned && !path {
                        let range = id.span().byte_range();
                        cfgs.push((base + range.start, base + range.end, "all()"));
                    }
                }
                TokenTree::Ident(id)
                    if mode == Mode::Apple && (id == "target_os" || id == "target_vendor") =>
                {
                    let assigned =
                        matches!(toks.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == '=');
                    let value = match toks.get(i + 2) {
                        Some(TokenTree::Literal(l)) => l.to_string(),
                        _ => String::new(),
                    };
                    let holds = (id == "target_os" && value == "\"macos\"")
                        || (id == "target_vendor" && value == "\"apple\"");
                    if assigned && holds {
                        let start = id.span().byte_range().start;
                        let end = toks[i + 2].span().byte_range().end;
                        cfgs.push((base + start, base + end, "all()"));
                    }
                }
                TokenTree::Group(g) => cfg_walk(g.stream(), base, mode, cfgs),
                _ => {}
            }
        }
    }
    let mut cfgs: Vec<(usize, usize, &str)> = Vec::new();
    let mut os_ext: Vec<(usize, usize, &str)> = Vec::new();
    fn walk(
        ts: TokenStream,
        base: usize,
        mode: Mode,
        abis: &mut Vec<(usize, usize, &'static str)>,
        cfgs: &mut Vec<(usize, usize, &'static str)>,
        os_ext: &mut Vec<(usize, usize, &'static str)>,
        links: &mut Vec<(usize, usize, TokenStream)>,
    ) {
        let toks: Vec<TokenTree> = ts.into_iter().collect();
        let mut i = 0;
        while i < toks.len() {
            match &toks[i] {
                TokenTree::Ident(id) if id == "cfg" || id == "cfg_attr" => {
                    let mut at = i + 1;
                    if matches!(toks.get(at), Some(TokenTree::Punct(p)) if p.as_char() == '!') {
                        at += 1;
                    }
                    if let Some(TokenTree::Group(g)) = toks.get(at)
                        && g.delimiter() == Delimiter::Parenthesis
                    {
                        cfg_walk(g.stream(), base, mode, cfgs);
                        i = at + 1;
                        continue;
                    }
                }
                TokenTree::Ident(id) if mode == Mode::Windows && id == "extern" => {
                    if let Some(TokenTree::Literal(l)) = toks.get(i + 1) {
                        let rep = match l.to_string().as_str() {
                            "\"system\"" => Some("\"win64\""),
                            "\"system-unwind\"" => Some("\"win64-unwind\""),
                            _ => None,
                        };
                        if let Some(r) = rep {
                            let range = l.span().byte_range();
                            abis.push((base + range.start, base + range.end, r));
                        }
                    }
                }
                // std::os::windows::ffi::{OsStringExt::from_wide, OsStrExt::encode_wide}
                // aren't in this target's std; the helper module has them
                TokenTree::Ident(id)
                    if mode == Mode::Windows && id == "std" && i + 15 < toks.len() =>
                {
                    let path: Vec<String> = toks[i..i + 16]
                        .iter()
                        .map(|t| match t {
                            TokenTree::Ident(x) => x.to_string(),
                            TokenTree::Punct(p) => p.as_char().to_string(),
                            _ => String::new(),
                        })
                        .collect();
                    let sep = |k: usize| path[k] == ":" && path[k + 1] == ":";
                    if sep(1)
                        && sep(4)
                        && sep(7)
                        && sep(10)
                        && sep(13)
                        && path[3] == "os"
                        && path[6] == "windows"
                        && path[9] == "ffi"
                    {
                        let rep = match (path[12].as_str(), path[15].as_str()) {
                            ("OsStringExt", "from_wide") => {
                                Some("crate::__ape_hostlink::from_wide")
                            }
                            ("OsStrExt", "encode_wide") => {
                                Some("crate::__ape_hostlink::encode_wide")
                            }
                            _ => None,
                        };
                        if let Some(r) = rep {
                            let start = id.span().byte_range().start;
                            let end = toks[i + 15].span().byte_range().end;
                            os_ext.push((base + start, base + end, r));
                            i += 16;
                            continue;
                        }
                    }
                }
                TokenTree::Ident(id) if mode == Mode::Windows && id == "link" => {
                    let bang =
                        matches!(toks.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == '!');
                    let group = match toks.get(i + 2) {
                        Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => {
                            Some(g)
                        }
                        _ => None,
                    };
                    if let (true, Some(g)) = (bang, group) {
                        // the path before it, `windows_link::` or `::windows_core::`
                        let mut j = i;
                        loop {
                            let colons = j >= 2
                                && matches!(&toks[j - 1], TokenTree::Punct(p) if p.as_char() == ':')
                                && matches!(&toks[j - 2], TokenTree::Punct(p) if p.as_char() == ':');
                            if !colons {
                                break;
                            }
                            j -= 2;
                            if j >= 1 && matches!(&toks[j - 1], TokenTree::Ident(_)) {
                                j -= 1;
                            } else {
                                break;
                            }
                        }
                        let start = toks[j].span().byte_range().start;
                        let mut end = g.span().byte_range().end;
                        let mut next = i + 3;
                        if let Some(TokenTree::Punct(p)) = toks.get(i + 3)
                            && p.as_char() == ';'
                        {
                            end = p.span().byte_range().end;
                            next += 1;
                        }
                        links.push((base + start, base + end, g.stream()));
                        i = next;
                        continue;
                    }
                }
                TokenTree::Group(g) => walk(g.stream(), base, mode, abis, cfgs, os_ext, links),
                _ => {}
            }
            i += 1;
        }
    }
    walk(
        tokens,
        src.base,
        mode,
        &mut abis,
        &mut cfgs,
        &mut os_ext,
        &mut links,
    );
    if !os_ext.is_empty() {
        plan.os_ext = true;
    }
    cfgs.extend(os_ext);
    let mut replaced: Vec<(usize, usize)> = Vec::new();
    for (s, e, ts) in links {
        let Ok(decl) = syn::parse2::<LinkDecl>(ts) else {
            continue;
        };
        if decl.sig.variadic.is_some() || decl.sig.receiver().is_some() {
            continue;
        }
        let lib = plan
            .explicit
            .entry(decl.dll.clone())
            .or_insert_with(|| Explicit::dll(&decl.dll));
        let libsym = lib.sym.clone();
        plan.edits
            .push((fi, s, e, link_wrapper(&decl, &libsym, x86)));
        replaced.push((s, e));
    }
    if !x86 {
        abis.clear();
    }
    for (s, e, r) in abis.into_iter().chain(cfgs) {
        if replaced.iter().any(|(rs, re)| s >= *rs && e <= *re) {
            continue;
        }
        plan.edits.push((fi, s, e, r.to_string()));
    }
}

// on one line, so the lines after it keep their numbers
fn link_wrapper(decl: &LinkDecl, libsym: &str, x86: bool) -> String {
    let abi = if x86 { "win64" } else { "C" };
    let name = &decl.sig.ident;
    let sym = decl.sym.clone().unwrap_or_else(|| name.to_string());
    let tys: Vec<String> = decl
        .sig
        .inputs
        .iter()
        .map(|a| match a {
            syn::FnArg::Typed(t) => tokens(&t.ty),
            syn::FnArg::Receiver(_) => unreachable!(),
        })
        .collect();
    let params: String = tys
        .iter()
        .enumerate()
        .map(|(i, t)| format!("a{i}: {t}, "))
        .collect();
    let ty_list = tys.join(", ");
    let ret = tokens(&decl.sig.output);
    let args: String = (0..tys.len()).map(|i| format!("a{i}, ")).collect();
    format!(
        "#[allow(warnings, clippy::all)] pub unsafe extern \"{abi}\" fn {name}({params}) {ret} {{ \
static SLOT: crate::__ape_hostlink::Slot = crate::__ape_hostlink::Slot::new(); \
unsafe {{ let f: unsafe extern \"{abi}\" fn({ty_list}) {ret} = \
crate::__ape_hostlink::__core::mem::transmute(SLOT.direct(crate::__ape_hostlink::{libsym}.as_ptr(), b\"{sym}\\0\", {nargs})); \
f({args}) }} }}",
        nargs = tys.len(),
    )
}
