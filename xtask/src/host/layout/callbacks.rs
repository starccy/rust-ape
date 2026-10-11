//! What the `#[host_callback]` functions given for a callback type need to be told.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::Host;
use super::analysis::{Analysis, host_prim, is_c_char, tokens, widened};
use super::model::{Model, Ty};

/// What to put on the `#[host_callback]` functions given for a callback type whose
/// parameters differ on some host, or are small integers, and the
/// structures such callbacks take by value, which libffi has to be told
/// about. Looked at before function pointer types become
/// ape::host::Callback.
pub fn callback_warnings(
    unix_file: &syn::File,
    win_file: Option<&syn::File>,
    xnu_file: Option<&syn::File>,
) -> (Vec<String>, BTreeSet<String>) {
    let u = Model::new(Host::Linux, unix_file);
    let win = win_file.map(|f| Model::new(Host::Win, f));
    let xnu = xnu_file.map(|f| Model::new(Host::Xnu, f));
    let others: Vec<&Model> = win.iter().chain(xnu.iter()).collect();
    let lens = HashMap::new();
    let mut a = Analysis {
        base: &u,
        others: others.clone(),
        memo: HashMap::new(),
        warnings: Vec::new(),
        lens: &lens,
        ffi_recs: BTreeSet::new(),
        unsupported: Vec::new(),
    };
    let mut out = Vec::new();
    for item in &unix_file.items {
        let syn::Item::Type(t) = item else { continue };
        let name = t.ident.to_string();
        let Some(uf) = fn_ptr(&t.ty) else { continue };
        let views: BTreeMap<Host, &syn::TypeFnPtr> = others
            .iter()
            .filter_map(|m| Some((m.host, fn_ptr(m.aliases.get(&name)?)?)))
            .collect();
        let uargs: Vec<&syn::Type> = uf.inputs.iter().map(|a| &a.ty).collect();
        let uret = match &uf.output {
            syn::ReturnType::Type(_, t) => Some(&**t),
            _ => None,
        };
        let is_ld = |ut: &syn::Type, i: Option<usize>| {
            u.resolve(ut) == Ty::Prim("u128")
                && views.iter().any(|(h, f)| {
                    let t = match i {
                        Some(i) => f.inputs.get(i).map(|a| &a.ty),
                        None => match &f.output {
                            syn::ReturnType::Type(_, t) => Some(&**t),
                            _ => None,
                        },
                    };
                    t.is_some_and(|t| {
                        others.iter().find(|m| m.host == *h).unwrap().resolve(t) == Ty::Prim("f64")
                    })
                })
        };
        let mut changed = false;
        let mut sigs = Vec::new();
        // a long double by value on Linux, or a structure laid out
        // differently by value anywhere, is only reachable through
        // libffi
        let mut ffi = false;
        let mut undescribed = Vec::new();
        for h in [Host::Linux, Host::Xnu, Host::Win] {
            let model = others.iter().find(|m| m.host == h).copied();
            let hf = views.get(&h);
            let mut tys = Vec::new();
            for (i, ut) in uargs
                .iter()
                .enumerate()
                .map(|(i, t)| (i, *t))
                .chain(uret.map(|t| (usize::MAX, t)))
            {
                let index = (i != usize::MAX).then_some(i);
                let declared = tokens(ut);
                let t = if is_ld(ut, index) {
                    if h == Host::Linux {
                        ffi = true;
                        "ape::host::layout::LongDouble".to_string()
                    } else {
                        "f64".to_string()
                    }
                } else if h != Host::Win
                    && matches!(u.resolve(ut), Ty::Prim(p) if widened(p).is_some())
                {
                    let Ty::Prim(p) = u.resolve(ut) else {
                        unreachable!()
                    };
                    if is_c_char(ut) {
                        "i32".to_string()
                    } else {
                        widened(p).unwrap().to_string()
                    }
                } else if let Ty::Rec(r) = u.resolve(ut) {
                    if h == Host::Linux && a.differs(&r) {
                        ffi = true;
                        if a.base.ffi_members(&r, 0, false).is_some() {
                            a.ffi_recs.insert(r.clone());
                        } else {
                            undescribed.push(r.clone());
                        }
                    }
                    declared.clone()
                } else if let (Some(model), Some(hf)) = (model, hf) {
                    let ht = match index {
                        Some(i) => hf.inputs.get(i).map(|a| &a.ty),
                        None => match &hf.output {
                            syn::ReturnType::Type(_, t) => Some(&**t),
                            _ => None,
                        },
                    };
                    match ht.and_then(|ht| host_prim(&u.resolve(ut), &model.resolve(ht))) {
                        Some(p) => p.to_string(),
                        None => declared.clone(),
                    }
                } else {
                    declared.clone()
                };
                changed |= t != declared || is_ld(ut, index);
                tys.push(t);
            }
            let ret = if uret.is_some() {
                format!(" -> {}", tys.pop().unwrap())
            } else {
                String::new()
            };
            sigs.push(format!("fn({}){ret}", tys.join(", ")));
        }
        if !undescribed.is_empty() {
            out.push(format!(
                "callback type {name} takes {} by value, which is laid out differently on some host and libffi can't describe; a host_callback for it can't be right everywhere",
                undescribed.join(", ")
            ));
            continue;
        }
        if changed || ffi {
            let how = if ffi { "ffi, " } else { "" };
            let why = if ffi {
                "passes a long double, or a structure laid out differently, by value, which a Rust function can't take on every host; put"
            } else {
                "takes other types on some host: put"
            };
            out.push(format!(
                "callback type {name} {why} #[ape::host_callback({how}sysv = {}, xnu = {}, win64 = {})] on the functions given for it",
                sigs[0], sigs[1], sigs[2]
            ));
        }
    }
    (out, a.ffi_recs)
}

// the fn type in `Option<unsafe extern "C" fn(..)>` or a bare one
fn fn_ptr(ty: &syn::Type) -> Option<&syn::TypeFnPtr> {
    match ty {
        syn::Type::FnPtr(f) => Some(f),
        syn::Type::Path(p) => {
            let seg = p.path.segments.last()?;
            let syn::PathArguments::AngleBracketed(a) = &seg.arguments else {
                return None;
            };
            match a.args.first()? {
                syn::GenericArgument::Type(t) => fn_ptr(t),
                _ => None,
            }
        }
        _ => None,
    }
}
