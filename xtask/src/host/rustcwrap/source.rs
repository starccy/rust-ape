//! A parsed source file and the byte-offset helpers the edits work in.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use proc_macro2::{Span, TokenStream};
use quote::ToTokens;
use syn::spanned::Spanned;

pub(super) struct Src {
    pub(super) path: PathBuf,
    pub(super) text: String,
    /// where syn's byte offsets start in `text`
    pub(super) base: usize,
    pub(super) file: syn::File,
}

pub(super) fn load(path: &Path) -> Result<Src> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut base = 0;
    let mut rest = text.as_str();
    if let Some(r) = rest.strip_prefix('\u{feff}') {
        base += 3;
        rest = r;
    }
    if rest.starts_with("#!") && !rest[2..].trim_start().starts_with('[') {
        let idx = rest.find('\n').unwrap_or(rest.len());
        base += idx;
    }
    let file = syn::parse_file(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Src {
        path: path.to_path_buf(),
        text,
        base,
        file,
    })
}

pub(super) fn attr_str(attrs: &[syn::Attribute], name: &str) -> Option<String> {
    attrs.iter().find_map(|a| {
        if !a.path().is_ident(name) {
            return None;
        }
        match &a.meta {
            syn::Meta::NameValue(nv) => match &nv.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        }
    })
}

// the path an include!() names, when it's a literal, concat!() and env!()
pub(super) fn eval_path(tokens: &TokenStream) -> Option<String> {
    let expr: syn::Expr = syn::parse2(tokens.clone()).ok()?;
    fn eval(e: &syn::Expr) -> Option<String> {
        match e {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Some(s.value()),
            syn::Expr::Macro(m) => {
                let name = m.mac.path.get_ident()?.to_string();
                let args = m
                    .mac
                    .parse_body_with(
                        syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
                    )
                    .ok()?;
                match name.as_str() {
                    "concat" => args
                        .iter()
                        .map(eval)
                        .collect::<Option<Vec<_>>>()
                        .map(|v| v.concat()),
                    "env" => std::env::var(eval(args.first()?)?).ok(),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    eval(&expr)
}

pub(super) fn items_at<'a>(items: &'a [syn::Item], inline: &[usize]) -> &'a [syn::Item] {
    let mut items = items;
    for &i in inline {
        match &items[i] {
            syn::Item::Mod(syn::ItemMod {
                content: Some((_, inner)),
                ..
            }) => items = inner,
            _ => return &[],
        }
    }
    items
}

// the items of a scope and where new items go: the end of the file, or
// the closing brace of an inline module
pub(super) fn scope_items<'a>(src: &'a Src, inline: &[usize]) -> (&'a [syn::Item], usize) {
    let mut items: &[syn::Item] = &src.file.items;
    let mut end = src.text.len();
    for &i in inline {
        match &items[i] {
            syn::Item::Mod(syn::ItemMod {
                content: Some((brace, inner)),
                ..
            }) => {
                end = src.base + brace.span.close().byte_range().start;
                items = inner;
            }
            _ => return (&[], end),
        }
    }
    (items, end)
}

pub(super) fn dir_for_path_attr(file: &Path, dir: &Path, top: bool) -> PathBuf {
    if top {
        file.parent().unwrap().to_path_buf()
    } else {
        dir.to_path_buf()
    }
}

// `#[link(name = "x", kind = "k")]`, or the same under a `cfg_attr`, the
// way crates for Apple frameworks write it (their cfg is off for this
// target, but the library is the one the code wants on that host)
pub(super) fn link_attr(a: &syn::Attribute) -> Option<(String, String)> {
    if a.path().is_ident("cfg_attr") {
        let mut found = None;
        let _ = a.parse_nested_meta(|m| {
            if m.path.is_ident("link") {
                let mut name = None;
                let mut kind = "dylib".to_string();
                m.parse_nested_meta(|m| {
                    if m.path.is_ident("name") {
                        name = Some(m.value()?.parse::<syn::LitStr>()?.value());
                    } else if m.path.is_ident("kind") {
                        kind = m.value()?.parse::<syn::LitStr>()?.value();
                    } else if m.input.peek(syn::Token![=]) {
                        let _: syn::Expr = m.value()?.parse()?;
                    }
                    Ok(())
                })?;
                if let Some(n) = name
                    && kind == "framework"
                {
                    found = Some((n, kind));
                }
            } else if m.input.peek(syn::Token![=]) {
                let _: syn::Expr = m.value()?.parse()?;
            } else if m.input.peek(syn::token::Paren) {
                m.parse_nested_meta(|m| {
                    // the predicate; skip over its arguments
                    if m.input.peek(syn::Token![=]) {
                        let _: syn::Expr = m.value()?.parse()?;
                    } else if m.input.peek(syn::token::Paren) {
                        let content;
                        syn::parenthesized!(content in m.input);
                        let _ = content.parse::<TokenStream>();
                    }
                    Ok(())
                })?;
            }
            Ok(())
        });
        return found;
    }
    if !a.path().is_ident("link") {
        return None;
    }
    let mut name = None;
    let mut kind = "dylib".to_string();
    let _ = a.parse_nested_meta(|m| {
        if m.path.is_ident("name") {
            name = Some(m.value()?.parse::<syn::LitStr>()?.value());
        } else if m.path.is_ident("kind") {
            kind = m.value()?.parse::<syn::LitStr>()?.value();
        } else if m.input.peek(syn::Token![=]) {
            let _: syn::Expr = m.value()?.parse()?;
        }
        Ok(())
    });
    Some((name?, kind))
}

pub(super) fn span_bytes(src: &Src, span: Span, attrs: &[syn::Attribute]) -> (usize, usize) {
    let r = span.byte_range();
    let mut start = r.start;
    for a in attrs {
        start = start.min(a.span().byte_range().start);
    }
    (src.base + start, src.base + r.end)
}

// keeps line numbers where they were
pub(super) fn blank(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' { '\n' } else { ' ' })
        .collect()
}

pub(super) fn tokens(t: impl ToTokens) -> String {
    t.to_token_stream().to_string()
}
