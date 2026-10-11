//! `#[host_library(..)]` on an `extern "C"` block.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Attribute, ForeignItem, Ident, ItemForeignMod, LitStr, Token, Type, Visibility, bracketed,
};

use crate::spec::{Sig, Spec};

/// `pub static ZLIB = linux ["libz.so.1"], macos [..], windows [..], bsd [..]`,
/// with `global,` after the `=` for RTLD_GLOBAL.
pub struct Args {
    vis: Visibility,
    name: Ident,
    global: bool,
    hosts: Vec<(Ident, Vec<LitStr>)>,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let vis: Visibility = input.parse()?;
        input.parse::<Token![static]>()?;
        let name: Ident = input.parse()?;
        input.parse::<Token![=]>()?;
        let mut global = false;
        let mut hosts: Vec<(Ident, Vec<LitStr>)> = Vec::new();
        while !input.is_empty() {
            let os: Ident = input.parse()?;
            if os == "global" {
                if global || !hosts.is_empty() {
                    return Err(syn::Error::new(
                        os.span(),
                        "`global` goes once, right after the `=`",
                    ));
                }
                global = true;
            } else {
                if !matches!(
                    os.to_string().as_str(),
                    "linux" | "macos" | "windows" | "bsd"
                ) {
                    return Err(syn::Error::new(
                        os.span(),
                        "expected linux, macos, windows or bsd",
                    ));
                }
                if hosts.iter().any(|(o, _)| *o == os) {
                    return Err(syn::Error::new(
                        os.span(),
                        format!("`{os}` names are given twice"),
                    ));
                }
                let inner;
                bracketed!(inner in input);
                let names = inner.parse_terminated(<LitStr as Parse>::parse, Token![,])?;
                hosts.push((os, names.into_iter().collect()));
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        if hosts.is_empty() {
            return Err(syn::Error::new(
                name.span(),
                "name the library on at least one host: linux [..], macos [..], windows [..] or bsd [..]",
            ));
        }
        Ok(Args {
            vis,
            name,
            global,
            hosts,
        })
    }
}

/// How a function is called: through a transmuted pointer, or libffi.
#[derive(Clone, Copy, PartialEq)]
enum Mech {
    Direct,
    Ffi,
}

/// The declaration as the user wrote it, names given to `_` parameters.
struct Decl {
    attrs: Vec<Attribute>,
    vis: Visibility,
    name: Ident,
    sym: LitStr,
    args: Vec<(Ident, Type)>,
    ret: Option<Type>,
}

impl Decl {
    fn sig(&self) -> Sig {
        Sig {
            args: self.args.iter().map(|(_, t)| (t.clone(), None)).collect(),
            ret: self.ret.clone(),
        }
    }

    fn names(&self) -> Vec<&Ident> {
        self.args.iter().map(|(n, _)| n).collect()
    }

    fn types(&self) -> Vec<&Type> {
        self.args.iter().map(|(_, t)| t).collect()
    }

    fn ret_arrow(&self) -> TokenStream {
        arrow(&self.ret)
    }
}

fn arrow(ret: &Option<Type>) -> TokenStream {
    match ret {
        Some(t) => quote!(-> #t),
        None => quote!(),
    }
}

/// `()` or `(T)`, a type either way.
fn paren(ret: &Option<Type>) -> TokenStream {
    match ret {
        Some(t) => quote!((#t)),
        None => quote!(()),
    }
}

/// The declaration's parameters, `_` named after its position.
fn params(sig: &syn::Signature) -> syn::Result<Vec<(Ident, Type)>> {
    let mut out = Vec::new();
    for (i, a) in sig.inputs.iter().enumerate() {
        let syn::FnArg::Typed(t) = a else {
            return Err(syn::Error::new(a.span(), "a host function has no `self`"));
        };
        let name = match &*t.pat {
            syn::Pat::Ident(p) => p.ident.clone(),
            syn::Pat::Wild(w) => Ident::new(&format!("__ape_arg{i}"), w.span()),
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "a parameter here is a name or `_`",
                ));
            }
        };
        out.push((name, (*t.ty).clone()));
    }
    Ok(out)
}

/// Takes `#[host(..)]` and `#[link_name = ".."]` out of a declaration's
/// attributes, leaving the rest for the generated item.
fn take_attrs(attrs: &mut Vec<Attribute>, name: &Ident) -> syn::Result<(Option<Spec>, LitStr)> {
    let mut spec = None;
    let mut sym = None;
    let mut keep = Vec::new();
    for a in attrs.drain(..) {
        if a.path().is_ident("host") {
            if spec.is_some() {
                return Err(syn::Error::new(a.span(), "one #[host(..)] per function"));
            }
            spec = Some(a.parse_args::<Spec>()?);
        } else if a.path().is_ident("link_name") {
            let syn::Meta::NameValue(nv) = &a.meta else {
                return Err(syn::Error::new(a.span(), "expected #[link_name = \"..\"]"));
            };
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
            else {
                return Err(syn::Error::new(a.span(), "expected #[link_name = \"..\"]"));
            };
            sym = Some(s.clone());
        } else {
            keep.push(a);
        }
    }
    *attrs = keep;
    Ok((
        spec,
        sym.unwrap_or_else(|| LitStr::new(&name.to_string(), name.span())),
    ))
}

pub fn expand(args: Args, mut block: ItemForeignMod) -> TokenStream {
    let lib = &args.name;
    let mut out = TokenStream::new();
    let mut errors: Vec<syn::Error> = Vec::new();

    if let Some(abi) = &block.abi.name
        && abi.value() != "C"
    {
        errors.push(syn::Error::new(
            abi.span(),
            "a host library is declared `extern \"C\"`; the attribute calls it the host's way",
        ));
    }

    // the static that loads the library; the block's own attributes, doc
    // comments mostly, go on it
    let hosts = args
        .hosts
        .iter()
        .map(|(os, names)| quote!(.#os(&[#(#names),*])));
    let global = args.global.then(|| quote!(.global()));
    let vis = &args.vis;
    let battrs = std::mem::take(&mut block.attrs);
    out.extend(quote! {
        #(#battrs)*
        #vis static #lib: ::ape::host::LazyLibrary = ::ape::host::LazyLibrary::new()
            #(#hosts)* #global;
    });

    for item in block.items {
        match item {
            ForeignItem::Fn(mut f) => {
                let name = f.sig.ident.clone();
                let r = take_attrs(&mut f.attrs, &name).and_then(|(spec, sym)| {
                    if !f.sig.generics.params.is_empty() {
                        return Err(syn::Error::new(f.sig.generics.span(), "a host function has no generics"));
                    }
                    let decl = Decl { attrs: f.attrs.clone(), vis: f.vis.clone(), name: name.clone(), sym, args: params(&f.sig)?, ret: ret_of(&f.sig.output) };
                    if f.sig.variadic.is_some() {
                        if spec.is_some() {
                            return Err(syn::Error::new(f.sig.span(), "a variadic function takes no #[host(..)]; it's called as declared, see host_variadic!"));
                        }
                        return Ok(variadic(lib, &decl));
                    }
                    match spec {
                        None => Ok(plain(lib, &decl)),
                        Some(s) if s.unsupported.is_some() => Err(syn::Error::new(
                            f.sig.ident.span(),
                            format!("{name} can't be called as declared on every host: {}", s.unsupported.unwrap().value()),
                        )),
                        Some(s) => {
                            let full = s.fill(&decl.sig(), f.sig.span())?;
                            Ok(converted(lib, &decl, &full))
                        }
                    }
                });
                match r {
                    Ok(t) => out.extend(t),
                    Err(e) => errors.push(e),
                }
            }
            ForeignItem::Static(mut s) => {
                let name = s.ident.clone();
                match take_attrs(&mut s.attrs, &name) {
                    Ok((Some(_), _)) => {
                        errors.push(syn::Error::new(s.span(), "a variable takes no #[host(..)]"))
                    }
                    Ok((None, sym)) => out.extend(variable(lib, &s, &sym)),
                    Err(e) => errors.push(e),
                }
            }
            other => errors.push(syn::Error::new(
                other.span(),
                "a host library declares functions and statics only",
            )),
        }
    }
    for e in errors {
        out.extend(e.to_compile_error());
    }
    out
}

fn ret_of(output: &syn::ReturnType) -> Option<Type> {
    match output {
        syn::ReturnType::Default => None,
        syn::ReturnType::Type(_, t) => Some((**t).clone()),
    }
}

/// A function called as declared, with the host's convention.
fn plain(lib: &Ident, d: &Decl) -> TokenStream {
    let Decl {
        attrs,
        vis,
        name,
        sym,
        ..
    } = d;
    let (anames, atys, ret) = (d.names(), d.types(), d.ret_arrow());
    let n = anames.len();
    quote! {
        #(#attrs)*
        #[allow(non_snake_case, clippy::missing_safety_doc, clippy::too_many_arguments)]
        #vis unsafe fn #name(#(#anames: #atys),*) #ret {
            static __APE_PTR: ::core::sync::atomic::AtomicUsize = ::core::sync::atomic::AtomicUsize::new(0);
            let __ape_p = #lib.resolve_n(&__APE_PTR, #sym, Some(#n));
            #[cfg(target_arch = "x86_64")]
            if ::ape::is_windows() {
                let __ape_f = unsafe {
                    ::core::mem::transmute::<usize, unsafe extern "win64" fn(#(#atys),*) #ret>(__ape_p)
                };
                return unsafe { __ape_f(#(#anames),*) };
            }
            let __ape_f = unsafe {
                ::core::mem::transmute::<usize, unsafe extern "C" fn(#(#atys),*) #ret>(__ape_p)
            };
            let __ape_r = unsafe { __ape_f(#(#anames),*) };
            ::ape::host::after_host_call();
            __ape_r
        }
    }
}

/// A function whose arguments and return value are converted to what each
/// host's convention takes, see ape::host::layout.
fn converted(lib: &Ident, d: &Decl, full: &crate::spec::Full) -> TokenStream {
    let Decl {
        attrs,
        vis,
        name,
        sym,
        ..
    } = d;
    let (anames, atys, ret) = (d.names(), d.types(), d.ret_arrow());
    let n = anames.len();
    let mech = if full.ffi { Mech::Ffi } else { Mech::Direct };
    let win = call(mech, "win64", d, &full.win64);
    let sysv = call(mech, "C", d, &full.sysv);
    let xnu = if mech == Mech::Direct && n >= 9 {
        packed(d, &full.xnu)
    } else {
        call(mech, "C", d, &full.xnu)
    };
    quote! {
        #(#attrs)*
        #[allow(non_snake_case, clippy::missing_safety_doc, clippy::too_many_arguments, unused_mut)]
        #vis unsafe fn #name(#(#anames: #atys),*) #ret {
            static __APE_PTR: ::core::sync::atomic::AtomicUsize = ::core::sync::atomic::AtomicUsize::new(0);
            let __ape_p = #lib.resolve_n(&__APE_PTR, #sym, Some(#n));
            #[cfg(target_arch = "x86_64")]
            if ::ape::is_windows() {
                return #win;
            }
            #[cfg(target_arch = "aarch64")]
            if ::ape::is_xnu() {
                let __ape_r = #xnu;
                ::ape::host::after_host_call();
                return __ape_r;
            }
            let __ape_r = #sysv;
            ::ape::host::after_host_call();
            __ape_r
        }
    }
}

/// `None`, or `Some(n as usize)` for a pointer with a length parameter.
fn len_tokens(len: &Option<Ident>) -> TokenStream {
    match len {
        Some(n) => quote!(Some(#n as usize)),
        None => quote!(None),
    }
}

/// Each argument converted to the host's type, before any name is reused,
/// since a length can come after the pointer it's for.
fn to_host(d: &Decl, h: &Sig) -> TokenStream {
    let anames = d.names();
    let conv = d.args.iter().zip(&h.args).map(|((a, ty), (hty, len))| {
        let len = len_tokens(len);
        quote!(<#ty as ::ape::host::layout::HostArg<#hty>>::to_host(#a, #len))
    });
    quote!(let (#(#anames,)*) = (#(#conv,)*);)
}

fn to_host_mut(d: &Decl, h: &Sig) -> TokenStream {
    let anames = d.names();
    let conv = d.args.iter().zip(&h.args).map(|((a, ty), (hty, len))| {
        let len = len_tokens(len);
        quote!(<#ty as ::ape::host::layout::HostArg<#hty>>::to_host(#a, #len))
    });
    quote!(let (#(mut #anames,)*) = (#(#conv,)*);)
}

fn back(d: &Decl, h: &Sig) -> TokenStream {
    let items = d.args.iter().zip(&h.args).map(
        |((a, ty), (hty, _))| quote!(<#ty as ::ape::host::layout::HostArg<#hty>>::back(#a.1);),
    );
    quote!(#(#items)*)
}

/// The return value back in the declared type.
fn from_host(d: &Decl, h: &Sig, r: &Ident) -> TokenStream {
    match (&d.ret, &h.ret) {
        (Some(ret), Some(hret)) => {
            quote!(<#ret as ::ape::host::layout::HostRet<#hret>>::from_host(#r))
        }
        _ => quote!(#r),
    }
}

fn call(mech: Mech, abi: &str, d: &Decl, h: &Sig) -> TokenStream {
    let abi = LitStr::new(abi, Span::call_site());
    let anames = d.names();
    let htys: Vec<&Type> = h.args.iter().map(|(t, _)| t).collect();
    let hret = arrow(&h.ret);
    let hret_paren = paren(&h.ret);
    let r = Ident::new("__ape_r", Span::call_site());
    let ret = from_host(d, h, &r);
    let back = back(d, h);
    match mech {
        Mech::Direct => {
            let conv = to_host(d, h);
            quote! {{
                let __ape_f = unsafe {
                    ::core::mem::transmute::<usize, unsafe extern #abi fn(#(#htys),*) #hret>(__ape_p)
                };
                #conv
                let __ape_r = unsafe { __ape_f(#(#anames.0),*) };
                #back
                #ret
            }}
        }
        Mech::Ffi => {
            let conv = to_host_mut(d, h);
            quote! {{
                static __APE_CIF: ::std::sync::OnceLock<::ape::host::ffi::Cif> = ::std::sync::OnceLock::new();
                let __ape_cif = __APE_CIF.get_or_init(|| ::ape::host::ffi::Cif::new(
                    &[#(<#htys as ::ape::host::ffi::FfiKind>::kind()),*],
                    <#hret_paren as ::ape::host::ffi::FfiKind>::kind()));
                #conv
                let __ape_r: #hret_paren = unsafe {
                    __ape_cif.call(__ape_p, &[#(&mut #anames.0 as *mut #htys as *mut ::core::ffi::c_void),*])
                };
                #back
                #ret
            }}
        }
    }
}

/// A call on macOS with nine or more arguments: the ones past the registers
/// may be packed on the stack, which the standard convention our code is
/// built for doesn't do. Whether they are is settled at compile time from
/// the types, so a call that needs no packing costs nothing more.
fn packed(d: &Decl, h: &Sig) -> TokenStream {
    let anames = d.names();
    let atys = d.types();
    let htys: Vec<&Type> = h.args.iter().map(|(t, _)| t).collect();
    let hret_paren = paren(&h.ret);
    let r = Ident::new("__ape_r", Span::call_site());
    let ret = from_host(d, h, &r);
    let conv = to_host(d, h);
    let back = back(d, h);
    let direct = call(Mech::Direct, "C", d, h);
    let msgs = anames.iter().map(|a| {
        format!("host_library: {a} is wider than a register, which a call with more than eight arguments can't lay out for macOS here; declare the function with #[host(ffi, ..)]")
    });
    quote! {{
        #[cfg(target_arch = "aarch64")]
        const __APE_PACKS: bool = {
            use ::ape::host::XnuKind as _;
            ::ape::host::xnu_packs(&[#((
                ::ape::host::Kind::<#htys>::FLOAT,
                ::core::mem::size_of::<#atys>(),
                ::core::mem::align_of::<#atys>(),
            )),*])
        };
        #[cfg(not(target_arch = "aarch64"))]
        const __APE_PACKS: bool = false;
        if __APE_PACKS {
            #(const _: () = assert!(::core::mem::size_of::<#atys>() <= 8, #msgs);)*
            #conv
            #[cfg(target_arch = "aarch64")]
            let (__ape_x0, __ape_d0) = unsafe {
                use ::ape::host::XnuKind as _;
                ::ape::host::xnu_pcall(__ape_p, &[#((
                    ::ape::host::Kind::<#htys>::FLOAT,
                    ::ape::host::xnu_bits(&#anames.0),
                    ::core::mem::size_of::<#atys>(),
                    ::core::mem::align_of::<#atys>(),
                )),*])
            };
            #[cfg(not(target_arch = "aarch64"))]
            let (__ape_x0, __ape_d0) = (0u64, 0u64);
            #back
            let __ape_r: #hret_paren = <#hret_paren as ::ape::host::VarRet>::from_regs(__ape_x0, __ape_d0);
            #ret
        } else {
            #direct
        }
    }}
}

/// A variadic function becomes a module with the function pointer per
/// convention, which `host_variadic!` picks from.
fn variadic(lib: &Ident, d: &Decl) -> TokenStream {
    let Decl {
        attrs,
        vis,
        name,
        sym,
        ..
    } = d;
    let atys = d.types();
    let ret = d.ret_arrow();
    let ret_paren = paren(&d.ret);
    let n = atys.len();
    quote! {
        #(#attrs)*
        #[allow(non_snake_case)]
        #vis mod #name {
            #[allow(unused_imports)]
            use super::*;
            static __APE_PTR: ::core::sync::atomic::AtomicUsize = ::core::sync::atomic::AtomicUsize::new(0);
            /// The function, for calling on Linux, macOS and the BSDs.
            pub fn sysv() -> unsafe extern "C" fn(#(#atys,)* ...) #ret {
                let p = super::#lib.resolve_n(&__APE_PTR, #sym, Some(usize::MAX));
                unsafe { ::core::mem::transmute::<usize, unsafe extern "C" fn(#(#atys,)* ...) #ret>(p) }
            }
            /// The function, for calling on Windows. A call can pass any
            /// number of arguments, so it's the function itself, not the
            /// stub `ape::host::direct` would make for a count.
            #[cfg(target_arch = "x86_64")]
            pub fn win64() -> unsafe extern "win64" fn(#(#atys,)* ...) #ret {
                let p = super::#lib.resolve_n(&__APE_PTR, #sym, Some(usize::MAX));
                unsafe { ::core::mem::transmute::<usize, unsafe extern "win64" fn(#(#atys,)* ...) #ret>(p) }
            }
            /// Calls the function on macOS, where the variadic arguments
            /// go on the stack. `args` is every argument, classified.
            #[cfg(target_arch = "aarch64")]
            pub unsafe fn xnu(args: &[(bool, u64)]) -> #ret_paren {
                const FIXED: usize = #n;
                let p = super::#lib.resolve_n(&__APE_PTR, #sym, Some(usize::MAX));
                let (x0, d0) = unsafe { ::ape::host::xnu_vcall(p, args, FIXED) };
                <#ret_paren as ::ape::host::VarRet>::from_regs(x0, d0)
            }
        }
    }
}

/// A variable of the library: a function returning where it lives.
fn variable(lib: &Ident, s: &syn::ForeignItemStatic, sym: &LitStr) -> TokenStream {
    let (attrs, vis, name, ty) = (&s.attrs, &s.vis, &s.ident, &s.ty);
    quote! {
        #(#attrs)*
        #[allow(non_snake_case)]
        #vis fn #name() -> *mut #ty {
            static __APE_PTR: ::core::sync::atomic::AtomicUsize = ::core::sync::atomic::AtomicUsize::new(0);
            let mut p = __APE_PTR.load(::core::sync::atomic::Ordering::Acquire);
            if p == 0 {
                let lib = #lib.load().unwrap_or_else(|e| panic!("{e}"));
                p = lib.sym(#sym)
                    .unwrap_or_else(|| panic!("{} has no {}", lib.name(), #sym)) as usize;
                __APE_PTR.store(p, ::core::sync::atomic::Ordering::Release);
            }
            p as *mut #ty
        }
    }
}
