//! The `#[host(..)]` description of a signature per calling convention,
//! shared by the two attributes.

use proc_macro2::Span;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitStr, Token, Type, bracketed, parenthesized};

/// One convention's view of a signature: each parameter's type, with the
/// parameter that holds its length when it points at an array, and the
/// return type.
#[derive(Clone)]
pub struct Sig {
    pub args: Vec<(Type, Option<Ident>)>,
    pub ret: Option<Type>,
}

impl Parse for Sig {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<Token![fn]>()?;
        let content;
        parenthesized!(content in input);
        let mut args = Vec::new();
        while !content.is_empty() {
            let ty: Type = content.parse()?;
            let len = if content.peek(syn::token::Bracket) {
                let inner;
                bracketed!(inner in content);
                Some(inner.parse::<Ident>()?)
            } else {
                None
            };
            args.push((ty, len));
            if !content.is_empty() {
                content.parse::<Token![,]>()?;
            }
        }
        let ret = if input.peek(Token![->]) {
            input.parse::<Token![->]>()?;
            Some(input.parse()?)
        } else {
            None
        };
        Ok(Sig { args, ret })
    }
}

/// What `#[host(..)]` said: `ffi`, a signature per convention, or
/// `unsupported = "why"`. Each at most once, in any order.
#[derive(Default)]
pub struct Spec {
    pub ffi: bool,
    pub sysv: Option<Sig>,
    pub xnu: Option<Sig>,
    pub win64: Option<Sig>,
    pub unsupported: Option<LitStr>,
}

impl Parse for Spec {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut spec = Spec::default();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            let twice =
                |what: &str| syn::Error::new(key.span(), format!("`{what}` is given twice"));
            match key.to_string().as_str() {
                "ffi" => {
                    if spec.ffi {
                        return Err(twice("ffi"));
                    }
                    spec.ffi = true;
                }
                "unsupported" => {
                    input.parse::<Token![=]>()?;
                    if spec.unsupported.is_some() {
                        return Err(twice("unsupported"));
                    }
                    spec.unsupported = Some(input.parse()?);
                }
                conv @ ("sysv" | "xnu" | "win64") => {
                    input.parse::<Token![=]>()?;
                    let sig: Sig = input.parse()?;
                    let slot = match conv {
                        "sysv" => &mut spec.sysv,
                        "xnu" => &mut spec.xnu,
                        _ => &mut spec.win64,
                    };
                    if slot.is_some() {
                        return Err(twice(conv));
                    }
                    *slot = Some(sig);
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown `{other}`: expected ffi, sysv = fn(..), xnu = fn(..), win64 = fn(..)"
                        ),
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(spec)
    }
}

/// The three conventions, the ones not given taking the declaration.
pub struct Full {
    pub ffi: bool,
    pub sysv: Sig,
    pub xnu: Sig,
    pub win64: Sig,
}

impl Spec {
    pub fn is_empty(&self) -> bool {
        !self.ffi
            && self.sysv.is_none()
            && self.xnu.is_none()
            && self.win64.is_none()
            && self.unsupported.is_none()
    }

    pub fn fill(self, declared: &Sig, span: Span) -> syn::Result<Full> {
        let n = declared.args.len();
        for (conv, sig) in [
            ("sysv", &self.sysv),
            ("xnu", &self.xnu),
            ("win64", &self.win64),
        ] {
            if let Some(s) = sig {
                if s.args.len() != n {
                    return Err(syn::Error::new(
                        span,
                        format!(
                            "`{conv}` lists {} parameters, the declaration has {n}",
                            s.args.len()
                        ),
                    ));
                }
                if s.ret.is_some() != declared.ret.is_some() {
                    return Err(syn::Error::new(
                        span,
                        format!(
                            "`{conv}` and the declaration disagree on whether there is a return value"
                        ),
                    ));
                }
            }
        }
        let or = |s: Option<Sig>| s.unwrap_or_else(|| declared.clone());
        Ok(Full {
            ffi: self.ffi,
            sysv: or(self.sysv),
            xnu: or(self.xnu),
            win64: or(self.win64),
        })
    }
}
