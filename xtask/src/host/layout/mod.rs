//! The per-host half of `cargo xtask host-bindings`.
//!
//! Windows keeps `long` at 32 bits and `wchar_t` at 16, and Microsoft's
//! `long double` is a `double`, as is Apple's on arm64. bindgen run on the
//! same header for each target says where that moves things: its layout
//! checks give each structure's size and field offsets as each compiler
//! has them. A structure whose layout differs anywhere becomes a block of
//! bytes the size of the largest, laid out as the host we run on lays it
//! out, with a getter and a setter per field that pick the offset and
//! width at runtime. A function whose parameters or return value differ
//! gets a `#[host(..)]` attribute with the types each convention really
//! takes, which `#[ape::host_library]` converts to and from, see
//! `ape::host::layout`. Small integers are widened there too, which
//! Apple's arm64 convention expects of the caller, and a `long double` or
//! a differing structure passed by value sends the call through libffi.
//!
//! The API is the Linux one throughout: a `long` is an `i64` to the
//! caller on every host, a `long double` an `ape::host::layout::LongDouble`.

mod analysis;
mod callbacks;
mod emit;
mod model;

pub use self::callbacks::callback_warnings;

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result};
use quote::ToTokens;

use self::analysis::{Analysis, fn_sig};
use self::emit::{emit_ffi_kind, emit_record};
use self::model::{Model, last_ident};

/// The hosts, numbered as `ape::host::layout::host()` numbers them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Host {
    Linux = 0,
    Win = 1,
    Xnu = 2,
}

impl Host {
    pub(super) fn name(self) -> &'static str {
        match self {
            Host::Linux => "Linux",
            Host::Win => "Windows",
            Host::Xnu => "macOS",
        }
    }
}

/// What the per-host bindings add to the Linux ones.
pub struct Dual {
    /// structures laid out differently, replaced wholesale
    pub converted: BTreeSet<String>,
    /// the replacements, and the libffi descriptions
    pub items: Vec<syn::Item>,
    /// `#[host(..)]` for each function whose types differ
    pub sigs: HashMap<String, syn::Attribute>,
    /// per function, the parameters (by index) and whether the return
    /// value are a `long double`, to be declared as `LongDouble`
    pub long_doubles: HashMap<String, (Vec<usize>, bool)>,
    pub warnings: Vec<String>,
}

/// `lens` says, per function and pointer parameter, which parameter holds
/// how many elements it points at (`--len f:p=n`).
pub fn analyze(
    unix_file: &syn::File,
    win_file: Option<&syn::File>,
    xnu_file: Option<&syn::File>,
    lens: &HashMap<(String, String), String>,
    callback_recs: &BTreeSet<String>,
) -> Result<Dual> {
    let unix = Model::new(Host::Linux, unix_file);
    let win = win_file.map(|f| Model::new(Host::Win, f));
    let xnu = xnu_file.map(|f| Model::new(Host::Xnu, f));
    let mut a = Analysis {
        base: &unix,
        others: win.iter().chain(xnu.iter()).collect(),
        memo: HashMap::new(),
        warnings: Vec::new(),
        lens,
        ffi_recs: callback_recs.clone(),
        unsupported: Vec::new(),
    };
    let names: BTreeSet<String> = unix.records.keys().cloned().collect();
    let mut converted = BTreeSet::new();
    for r in &names {
        if !unix.sizes.contains_key(r) {
            // opaque, or bindgen was told not to check layouts
            continue;
        }
        if a.differs(r) {
            converted.insert(r.clone());
        }
    }
    let mut items = Vec::new();
    if !converted.is_empty() {
        items.push(syn::parse_quote! {
            /// A pointer to a structure that follows the host's layout, to reach
            /// its fields through without a reference, which a field a host
            /// places at a smaller alignment couldn't be.
            #[repr(transparent)]
            #[derive(Clone, Copy, Debug)]
            pub struct At<T>(pub *mut T);
        });
    }
    for r in &converted {
        let text = emit_record(&mut a, r);
        let file: syn::File = syn::parse_str(&text)
            .with_context(|| format!("generated code for {r} doesn't parse:\n{text}"))?;
        items.extend(file.items);
    }
    let mut sigs = HashMap::new();
    let mut long_doubles = HashMap::new();
    let mut fnames: Vec<&String> = unix.fns.keys().collect();
    fnames.sort();
    for name in fnames {
        let uf = &unix.fns[name];
        let (attr, ld) = fn_sig(&mut a, uf);
        if let Some(attr) = attr {
            sigs.insert(name.clone(), attr);
        }
        if let Some(ld) = ld {
            long_doubles.insert(name.clone(), ld);
        }
    }
    for r in a.ffi_recs.clone() {
        let text = emit_ffi_kind(&mut a, &r);
        let file: syn::File = syn::parse_str(&text)
            .with_context(|| format!("generated code for {r} doesn't parse:\n{text}"))?;
        items.extend(file.items);
    }
    let warnings = std::mem::take(&mut a.warnings);
    Ok(Dual {
        converted,
        items,
        sigs,
        long_doubles,
        warnings,
    })
}

/// Drops what `dual` replaces: the converted structures, their impls and
/// their layout checks.
pub fn strip(file: &mut syn::File, dual: &Dual) {
    file.items.retain(|item| match item {
        syn::Item::Struct(s) => !dual.converted.contains(&s.ident.to_string()),
        syn::Item::Union(u) => !dual.converted.contains(&u.ident.to_string()),
        syn::Item::Impl(i) => last_ident(&i.self_ty).is_none_or(|n| !dual.converted.contains(&n)),
        syn::Item::Const(c) if c.ident == "_" => {
            let text = c.expr.to_token_stream().to_string();
            !dual.converted.iter().any(|r| {
                text.contains(&format!("\"Size of {r}\""))
                    || text.contains(&format!("\"Offset of field: {r}::"))
            })
        }
        _ => true,
    });
}

/// Declares the `long double` parameters and return values `dual` found
/// as `LongDouble`.
pub fn declare_long_doubles(f: &mut syn::ForeignItemFn, dual: &Dual) {
    let Some((args, ret)) = dual.long_doubles.get(&f.sig.ident.to_string()) else {
        return;
    };
    let ld: syn::Type = syn::parse_quote!(ape::host::layout::LongDouble);
    for (i, arg) in f.sig.inputs.iter_mut().enumerate() {
        if let (true, syn::FnArg::Typed(p)) = (args.contains(&i), arg) {
            *p.ty = ld.clone();
        }
    }
    if let (true, syn::ReturnType::Type(_, t)) = (*ret, &mut f.sig.output) {
        **t = ld;
    }
}
