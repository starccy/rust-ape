//! Comparing the hosts' models: which records differ, and the
//! `#[host(..)]` a function gets when its types do.

use std::collections::{BTreeSet, HashMap};

use quote::{ToTokens, quote};

use super::Host;
use super::model::{Model, Ty, last_ident};

pub(super) struct Analysis<'a> {
    pub(super) base: &'a Model,
    pub(super) others: Vec<&'a Model>,
    pub(super) memo: HashMap<(String, Host), bool>,
    pub(super) warnings: Vec<String>,
    pub(super) lens: &'a HashMap<(String, String), String>,
    // structures passed by value through libffi, wanting a FfiKind impl
    pub(super) ffi_recs: BTreeSet<String>,
    // what the function being looked at can't have converted, which
    // makes it a compile error to declare
    pub(super) unsupported: Vec<String>,
}

impl<'a> Analysis<'a> {
    pub(super) fn model(&self, h: Host) -> Option<&'a Model> {
        if h == Host::Linux {
            return Some(self.base);
        }
        self.others.iter().copied().find(|m| m.host == h)
    }

    pub(super) fn all(&self) -> Vec<&'a Model> {
        let mut v = vec![self.base];
        v.extend(self.others.iter().copied());
        v
    }

    // whether `rec` is laid out differently on any host
    pub(super) fn differs(&mut self, rec: &str) -> bool {
        let hosts: Vec<Host> = self.others.iter().map(|m| m.host).collect();
        hosts.into_iter().any(|h| self.differs_on(rec, h))
    }

    fn differs_on(&mut self, rec: &str, h: Host) -> bool {
        let key = (rec.to_string(), h);
        if let Some(&d) = self.memo.get(&key) {
            return d;
        }
        self.memo.insert(key.clone(), false);
        let d = self.differs_uncached(rec, h);
        self.memo.insert(key, d);
        d
    }

    fn differs_uncached(&mut self, rec: &str, h: Host) -> bool {
        let u = self.base;
        let Some(w) = self.model(h) else { return false };
        if u.sizes.get(rec) != w.sizes.get(rec) {
            return true;
        }
        let (Some(ur), Some(wr)) = (u.records.get(rec), w.records.get(rec)) else {
            return u.records.contains_key(rec) != w.records.contains_key(rec);
        };
        if ur.fields.len() != wr.fields.len() {
            return true;
        }
        for ((un, ut), (wn, wt)) in ur.fields.iter().zip(&wr.fields) {
            if un != wn {
                return true;
            }
            let key = (rec.to_string(), un.clone());
            if u.offsets.get(&key) != w.offsets.get(&key) {
                return true;
            }
            let (a, b) = (u.resolve(ut), w.resolve(wt));
            if self.ty_differs(&a, &b, h) {
                return true;
            }
        }
        let bu = u.bitfields.get(rec).cloned().unwrap_or_default();
        let bw = w.bitfields.get(rec).cloned().unwrap_or_default();
        if bu.len() != bw.len()
            || bu.iter().zip(&bw).any(|(a, b)| {
                a.name != b.name || a.unit != b.unit || a.offset != b.offset || a.width != b.width
            })
            || u.unit_offsets(rec) != w.unit_offsets(rec)
        {
            return true;
        }
        false
    }

    fn ty_differs(&mut self, a: &Ty, b: &Ty, h: Host) -> bool {
        match (a, b) {
            (Ty::Rec(x), Ty::Rec(y)) => x != y || self.differs_on(x, h),
            (Ty::Array(x, n), Ty::Array(y, m)) => n != m || self.ty_differs(x, y, h),
            _ => a != b,
        }
    }

    fn unsupported(&mut self, what: String) {
        self.warnings.push(format!(
            "{what}; declared #[host(unsupported)], which won't compile"
        ));
        self.unsupported.push(what);
    }

    // whether a `u128` here is a `long double`, which another host shows
    // as an f64
    pub(super) fn is_long_double(&self, ut: &syn::Type, others: &[(Host, &syn::Type)]) -> bool {
        self.base.resolve(ut) == Ty::Prim("u128")
            && others.iter().any(|(h, t)| {
                self.model(*h)
                    .is_some_and(|m| m.resolve(t) == Ty::Prim("f64"))
            })
    }
}

fn pointee(ty: &syn::Type) -> Option<(&syn::Type, bool)> {
    match ty {
        syn::Type::Ptr(p) => Some((
            &p.elem,
            matches!(p.mutability, syn::PointerMutability::Mut(_)),
        )),
        _ => None,
    }
}

fn is_wchar(ty: &syn::Type) -> bool {
    last_ident(ty).as_deref() == Some("wchar_t")
}

fn arg_name(a: &syn::FnArg) -> String {
    match a {
        syn::FnArg::Typed(p) => p.pat.to_token_stream().to_string(),
        _ => String::new(),
    }
}

fn arg_ty(a: &syn::FnArg) -> Option<&syn::Type> {
    match a {
        syn::FnArg::Typed(p) => Some(&p.ty),
        _ => None,
    }
}

pub(super) fn tokens(t: &syn::Type) -> String {
    t.to_token_stream()
        .to_string()
        .replace(" :: ", "::")
        .replace(":: ", "::")
        .replace("* mut ", "*mut ")
        .replace("* const ", "*const ")
}

// a small integer as the register-sized one it's passed as
pub(super) fn widened(p: &str) -> Option<&'static str> {
    match p {
        "i8" | "i16" => Some("i32"),
        "u8" | "u16" | "bool" => Some("u32"),
        _ => None,
    }
}

// what `c_char` is on the target: signed on x86-64 and to Apple, so its
// widened form is an i32 whatever Rust calls it
pub(super) fn is_c_char(ty: &syn::Type) -> bool {
    last_ident(ty).as_deref() == Some("c_char")
}

// by-value pairs ape::host::layout converts
fn supported_value(u: &Ty, w: &Ty) -> bool {
    matches!(
        (u, w),
        (Ty::Prim("i64"), Ty::Prim("i32"))
            | (Ty::Prim("u64"), Ty::Prim("u32"))
            | (Ty::Prim("u128"), Ty::Prim("f64"))
            | (Ty::Prim("i32"), Ty::Prim("u16"))
    )
}

// the type a value of `u` is read as on another host, when it differs.
// a change of sign alone isn't one: an enum is `unsigned` to gcc and
// clang and `int` to msvc, and the bits pass the same either way
pub(super) fn host_prim(u: &Ty, w: &Ty) -> Option<&'static str> {
    match (u, w) {
        (Ty::Prim(a), Ty::Prim(b)) if a != b && !same_width(a, b) => Some(b),
        _ => None,
    }
}

fn same_width(a: &str, b: &str) -> bool {
    matches!(
        (a, b),
        ("i32", "u32")
            | ("u32", "i32")
            | ("i64", "u64")
            | ("u64", "i64")
            | ("i16", "u16")
            | ("u16", "i16")
            | ("i8", "u8")
            | ("u8", "i8")
    )
}

struct Branch {
    pub(super) args: Vec<proc_macro2::TokenStream>,
    pub(super) ret: Option<proc_macro2::TokenStream>,
    changed: bool,
}

// the `#[host(..)]` for a function whose types differ anywhere, and its
// long double parameters
pub(super) fn fn_sig(
    a: &mut Analysis,
    uf: &syn::ForeignItemFn,
) -> (Option<syn::Attribute>, Option<(Vec<usize>, bool)>) {
    let name = uf.sig.ident.to_string();
    a.unsupported.clear();
    let uargs: Vec<&syn::Type> = uf.sig.inputs.iter().filter_map(arg_ty).collect();
    let unames: Vec<String> = uf.sig.inputs.iter().map(arg_name).collect();
    let uret = match &uf.sig.output {
        syn::ReturnType::Type(_, t) => Some(&**t),
        _ => None,
    };
    // the other hosts' view of each parameter and the return value
    let mut per_host: Vec<(Host, Vec<&syn::Type>, Option<&syn::Type>)> = Vec::new();
    let others: Vec<&Model> = a.others.clone();
    for m in others {
        let Some(f) = m.fns.get(&name) else {
            a.warnings.push(format!(
                "{name} isn't in the {} bindings, left as on Linux",
                m.host.name()
            ));
            continue;
        };
        let args: Vec<&syn::Type> = f.sig.inputs.iter().filter_map(arg_ty).collect();
        if args.len() != uargs.len() {
            a.unsupported(format!(
                "{name} takes different parameters on {}",
                m.host.name()
            ));
            continue;
        }
        let ret = match &f.sig.output {
            syn::ReturnType::Type(_, t) => Some(&**t),
            _ => None,
        };
        per_host.push((m.host, args, ret));
    }
    let view = |i: Option<usize>| -> Vec<(Host, &syn::Type)> {
        per_host
            .iter()
            .filter_map(|(h, args, ret)| {
                Some((
                    *h,
                    match i {
                        Some(i) => args[i],
                        None => (*ret)?,
                    },
                ))
            })
            .collect()
    };
    let mut ld_args = Vec::new();
    let mut ld_ret = false;
    let mut ffi = false;
    for (i, ut) in uargs.iter().enumerate() {
        if a.is_long_double(ut, &view(Some(i))) {
            ld_args.push(i);
            ffi = true;
        } else if let Ty::Rec(r) = a.base.resolve(ut)
            && a.differs(&r)
        {
            ffi = true;
        }
    }
    if let Some(ut) = uret {
        if a.is_long_double(ut, &view(None)) {
            ld_ret = true;
            ffi = true;
        } else if let Ty::Rec(r) = a.base.resolve(ut)
            && a.differs(&r)
        {
            ffi = true;
        }
    }
    if ffi {
        // everything by value has to be describable to libffi
        for t in uargs.iter().chain(uret.iter()) {
            if let Ty::Rec(r) = a.base.resolve(t) {
                if a.base.ffi_members(&r, 0, false).is_none() {
                    a.unsupported(format!(
                        "{name} passes {r} by value, which libffi can't describe"
                    ));
                    ffi = false;
                } else {
                    a.ffi_recs.insert(r);
                }
            }
        }
    }
    let ld = (!ld_args.is_empty() || ld_ret).then(|| (ld_args.clone(), ld_ret));
    let ld_ty = quote!(ape::host::layout::LongDouble);

    let mut branches = Vec::new();
    for h in [Host::Linux, Host::Xnu, Host::Win] {
        let model = a.model(h);
        let hv = per_host.iter().find(|(x, ..)| *x == h);
        let mut b = Branch {
            args: Vec::new(),
            ret: None,
            changed: false,
        };
        for (i, ut) in uargs.iter().enumerate() {
            let len = a.lens.get(&(name.clone(), unames[i].clone())).cloned();
            if let Some(l) = &len
                && h == Host::Linux
                && !unames.contains(l)
            {
                a.warnings.push(format!(
                    "{name}: --len names {l}, which isn't one of its parameters"
                ));
            }
            let u = a.base.resolve(ut);
            let declared = if ld_args.contains(&i) {
                ld_ty.clone()
            } else {
                ut.to_token_stream()
            };
            let t = if ld_args.contains(&i) {
                if h == Host::Linux {
                    declared.clone()
                } else {
                    quote!(f64)
                }
            } else if h != Host::Win && matches!(u, Ty::Prim(p) if widened(p).is_some()) {
                let Ty::Prim(p) = u else { unreachable!() };
                let w = if is_c_char(ut) {
                    "i32"
                } else {
                    widened(p).unwrap()
                };
                let w: syn::Type = syn::parse_str(w).unwrap();
                quote!(#w)
            } else if let (Some(model), Some((_, args, _))) = (model, hv) {
                host_arg(a, model, &name, i, ut, args[i], len.as_deref())
                    .unwrap_or_else(|| declared.clone())
            } else {
                declared.clone()
            };
            b.changed |= t.to_string() != declared.to_string();
            b.args.push(t);
        }
        if let Some(ut) = uret {
            let declared = if ld_ret {
                ld_ty.clone()
            } else {
                ut.to_token_stream()
            };
            let t = if ld_ret {
                if h == Host::Linux {
                    declared.clone()
                } else {
                    quote!(f64)
                }
            } else if let (Some(model), Some((_, _, Some(wt)))) = (model, hv) {
                let (u, w) = (a.base.resolve(ut), model.resolve(wt));
                if let Some(p) = host_prim(&u, &w) {
                    if !supported_value(&u, &w) {
                        a.unsupported(format!(
                            "{name} returns a {p} on {}, which isn't converted",
                            h.name()
                        ));
                    }
                    let p: syn::Type = syn::parse_str(p).unwrap();
                    quote!(#p)
                } else {
                    if let (Some((up, _)), Some((wp, _))) = (pointee(ut), pointee(wt))
                        && host_prim(&a.base.resolve(up), &model.resolve(wp)).is_some()
                    {
                        a.unsupported(format!(
                            "{name} returns a pointer to something sized differently on {}, which isn't converted",
                            h.name()
                        ));
                    }
                    declared.clone()
                }
            } else {
                declared.clone()
            };
            b.changed |= t.to_string() != declared.to_string();
            b.ret = Some(t);
        }
        branches.push(b);
    }
    if !a.unsupported.is_empty() {
        let why = a.unsupported.join("; ");
        let f: syn::ItemFn = syn::parse_quote!(
            #[host(unsupported = #why)]
            fn x() {}
        );
        return (f.attrs.into_iter().next(), ld);
    }
    if !ffi && !branches.iter().any(|b| b.changed) {
        return (None, ld);
    }
    let sig = |b: &Branch| {
        let args = &b.args;
        match &b.ret {
            Some(r) => quote!(fn(#(#args),*) -> #r),
            None => quote!(fn(#(#args),*)),
        }
    };
    let (s, x, w) = (sig(&branches[0]), sig(&branches[1]), sig(&branches[2]));
    let attr = if ffi {
        quote!(#[host(ffi, sysv = #s, xnu = #x, win64 = #w)])
    } else {
        quote!(#[host(sysv = #s, xnu = #x, win64 = #w)])
    };
    let f: syn::ItemFn = match syn::parse2(quote!(#attr fn x() {})) {
        Ok(f) => f,
        Err(e) => {
            a.warnings.push(format!(
                "{name}: the attribute made for it doesn't parse: {e}"
            ));
            return (None, ld);
        }
    };
    (f.attrs.into_iter().next(), ld)
}

// the type parameter `i` takes on `model`'s host, when it differs
fn host_arg(
    a: &mut Analysis,
    model: &Model,
    name: &str,
    i: usize,
    ut: &syn::Type,
    wt: &syn::Type,
    len: Option<&str>,
) -> Option<proc_macro2::TokenStream> {
    let hn = model.host.name();
    let (u, w) = (a.base.resolve(ut), model.resolve(wt));
    if let Some(p) = host_prim(&u, &w) {
        if !supported_value(&u, &w) {
            a.unsupported(format!(
                "{name}: parameter {i} is a {p} on {hn}, which isn't converted"
            ));
        }
        let p: syn::Type = syn::parse_str(p).unwrap();
        return Some(quote!(#p));
    }
    let ((up, umut), (wp, _)) = (pointee(ut)?, pointee(wt)?);
    let (pu, pw) = (a.base.resolve(up), model.resolve(wp));
    let p = host_prim(&pu, &pw)?;
    let n = len.map(|l| syn::Ident::new(l, proc_macro2::Span::call_site()));
    let n = n.map(|n| quote!([#n]));
    if is_wchar(up) || (pu == Ty::Prim("i32") && pw == Ty::Prim("u16")) {
        if umut {
            if n.is_none() {
                a.unsupported(format!(
                    "{name}: parameter {i} is a wchar_t buffer to fill, which needs --len {name}:<param>=<its length> to be converted"
                ));
                return None;
            }
            return Some(quote!(*mut u16 #n));
        }
        return Some(quote!(ape::host::layout::WStr16));
    }
    if !matches!(p, "i32" | "u32") {
        a.unsupported(format!(
            "{name}: parameter {i} points at a {p} on {hn}, which isn't converted"
        ));
        return None;
    }
    if n.is_none() {
        a.warnings.push(format!(
            "{name}: parameter {i} points at a `long`, taken to be a single value; for an array give --len {name}:<param>=<its length>"
        ));
    }
    let p: syn::Type = syn::parse_str(p).unwrap();
    Some(if umut {
        quote!(*mut #p #n)
    } else {
        quote!(*const #p #n)
    })
}
