//! `#[host_callback]` on a function.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::spanned::Spanned;
use syn::{Ident, ItemFn, Type};

use crate::spec::{Full, Sig, Spec};

/// The function as written: its parameters by name and type, and the
/// return type.
struct Decl {
    name: Ident,
    args: Vec<(Ident, Type)>,
    ret: Option<Type>,
}

impl Decl {
    fn names(&self) -> Vec<&Ident> {
        self.args.iter().map(|(n, _)| n).collect()
    }

    fn types(&self) -> Vec<&Type> {
        self.args.iter().map(|(_, t)| t).collect()
    }

    fn sig(&self) -> Sig {
        Sig {
            args: self.args.iter().map(|(_, t)| (t.clone(), None)).collect(),
            ret: self.ret.clone(),
        }
    }
}

fn arrow(ret: &Option<Type>) -> TokenStream {
    match ret {
        Some(t) => quote!(-> #t),
        None => quote!(),
    }
}

fn paren(ret: &Option<Type>) -> TokenStream {
    match ret {
        Some(t) => quote!((#t)),
        None => quote!(()),
    }
}

/// The return value as the host's convention wants it.
fn to_host(d: &Decl, h: &Sig, r: &Ident) -> TokenStream {
    match (&d.ret, &h.ret) {
        (Some(ret), Some(hret)) => {
            quote!(<#ret as ::ape::host::layout::HostArg<#hret>>::to_host(#r, None).0)
        }
        _ => quote!(#r),
    }
}

pub fn expand(spec: Spec, mut f: ItemFn) -> syn::Result<TokenStream> {
    if let Some(u) = &spec.unsupported {
        return Err(syn::Error::new(
            u.span(),
            "`unsupported` is for host_library declarations",
        ));
    }
    let sig = &f.sig;
    if let Some(c) = &sig.constness {
        return Err(syn::Error::new(c.span(), "a callback isn't const"));
    }
    if let Some(a) = &sig.asyncness {
        return Err(syn::Error::new(a.span(), "a callback isn't async"));
    }
    if let syn::Safety::Unsafe(u) = &sig.safety {
        return Err(syn::Error::new(
            u.span(),
            "write the callback as a safe fn; its body can have unsafe blocks",
        ));
    }
    if let Some(abi) = &sig.abi {
        return Err(syn::Error::new(
            abi.span(),
            "leave the ABI off; the attribute makes the function extern \"C\" and adds the host's conventions",
        ));
    }
    if !sig.generics.params.is_empty() {
        return Err(syn::Error::new(
            sig.generics.span(),
            "a callback has no generics",
        ));
    }
    if sig.variadic.is_some() {
        return Err(syn::Error::new(sig.span(), "a callback can't be variadic"));
    }
    let mut args = Vec::new();
    for (i, a) in sig.inputs.iter().enumerate() {
        let syn::FnArg::Typed(t) = a else {
            return Err(syn::Error::new(a.span(), "a callback has no `self`"));
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
        args.push((name, (*t.ty).clone()));
    }
    let ret = match &sig.output {
        syn::ReturnType::Default => None,
        syn::ReturnType::Type(_, t) => Some((**t).clone()),
    };
    let d = Decl {
        name: sig.ident.clone(),
        args,
        ret,
    };
    for (conv, s) in [
        ("sysv", &spec.sysv),
        ("xnu", &spec.xnu),
        ("win64", &spec.win64),
    ] {
        if let Some(s) = s
            && let Some((_, Some(len))) = s.args.iter().find(|(_, l)| l.is_some())
        {
            return Err(syn::Error::new(
                len.span(),
                format!(
                    "`{conv}`: a length belongs on host_library declarations, a callback has none"
                ),
            ));
        }
    }
    let plain = spec.is_empty();
    let full = spec.fill(&d.sig(), sig.span())?;
    let attrs = std::mem::take(&mut f.attrs);
    let vis = f.vis.clone();
    let body = &f.block;
    let name = &d.name;
    let (anames, atys, ret_arrow) = (d.names(), d.types(), arrow(&d.ret));

    let function = quote! {
        #(#attrs)*
        #[allow(non_snake_case)]
        #vis extern "C" fn #name(#(#anames: #atys),*) #ret_arrow {
            ::ape::host::after_host_call();
            #body
        }
    };

    let module = if full.ffi {
        ffi_module(&d, &vis, &full)
    } else if plain {
        let win64 = quote! {
            pub extern "win64" fn win64(#(#anames: #atys),*) #ret_arrow {
                super::#name(#(#anames),*)
            }
        };
        let xnu_entry = xnu_entry(&d, &d.sig());
        direct_module(
            &d,
            &vis,
            quote!(#vis use super::#name as sysv;),
            quote!(#vis use super::#name as xnu;),
            xnu_entry,
            win64,
        )
    } else {
        let sysv = wrapper(&d, &full.sysv, "sysv", "C");
        let xnu = wrapper(&d, &full.xnu, "xnu", "C");
        let win64 = wrapper(&d, &full.win64, "win64", "win64");
        let xnu_entry = xnu_entry(&d, &full.xnu);
        direct_module(&d, &vis, sysv, xnu, xnu_entry, win64)
    };
    Ok(quote!(#function #module))
}

/// The entry per convention that converts what the host passed and calls
/// the function.
fn wrapper(d: &Decl, h: &Sig, conv: &str, abi: &str) -> TokenStream {
    let conv = Ident::new(conv, Span::call_site());
    let abi = syn::LitStr::new(abi, Span::call_site());
    let name = &d.name;
    let anames = d.names();
    let htys: Vec<&Type> = h.args.iter().map(|(t, _)| t).collect();
    let hret = arrow(&h.ret);
    let from =
        d.args.iter().zip(&htys).map(
            |((a, ty), hty)| quote!(<#ty as ::ape::host::layout::HostRet<#hty>>::from_host(#a)),
        );
    let r = Ident::new("__ape_r", Span::call_site());
    let ret = to_host(d, h, &r);
    quote! {
        pub extern #abi fn #conv(#(#anames: #htys),*) #hret {
            let __ape_r = super::#name(#(#from),*);
            #ret
        }
    }
}

/// The module next to the function: the entries, and `ptr()` picking the
/// one for this host.
fn direct_module(
    d: &Decl,
    vis: &syn::Visibility,
    sysv: TokenStream,
    xnu: TokenStream,
    xnu_entry: Vec<TokenStream>,
    win64: TokenStream,
) -> TokenStream {
    let name = &d.name;
    quote! {
        #[allow(non_snake_case)]
        #vis mod #name {
            #[allow(unused_imports)]
            use super::*;
            #sysv
            #[cfg(target_arch = "aarch64")]
            #xnu
            #(
                #[cfg(target_arch = "aarch64")]
                #xnu_entry
            )*
            #[cfg(target_arch = "x86_64")]
            #win64
            /// The entry point to give a host library on this host.
            pub fn ptr() -> ::ape::host::Callback {
                #[cfg(target_arch = "x86_64")]
                if ::ape::is_windows() {
                    return ::ape::host::Callback(win64 as *const ::core::ffi::c_void);
                }
                #[cfg(target_arch = "aarch64")]
                if ::ape::is_xnu() {
                    return ::ape::host::Callback(xnu_entry());
                }
                ::ape::host::Callback(sysv as *const ::core::ffi::c_void)
            }
        }
    }
}

/// The macOS entry. With nine or more arguments Apple's arm64 may pack the
/// ones past the registers on the stack at their own sizes, where the
/// standard convention our code is built for has them in 8 byte slots;
/// whether it does is settled from the types at compile time, and only
/// then does the host get an entry that lays them out again.
fn xnu_entry(d: &Decl, h: &Sig) -> Vec<TokenStream> {
    if d.args.len() < 9 {
        return vec![quote! {
            pub fn xnu_entry() -> *const ::core::ffi::c_void {
                xnu as *const ::core::ffi::c_void
            }
        }];
    }
    let name = &d.name;
    let anames = d.names();
    let atys = d.types();
    let htys: Vec<&Type> = h.args.iter().map(|(t, _)| t).collect();
    let hret = arrow(&h.ret);
    let r = Ident::new("__ape_r", Span::call_site());
    let ret = to_host(d, h, &r);
    let msgs = anames.iter().map(|a| {
        format!("host_callback: {a} is wider than a register, which a callback with more than eight arguments can't take back from macOS here; declare it with ffi")
    });
    vec![
        quote! {
            const __APE_PACKS: bool = {
                use ::ape::host::XnuKind as _;
                ::ape::host::xnu_packs(&[#((
                    ::ape::host::Kind::<#htys>::FLOAT,
                    ::ape::host::xnu_min(::core::mem::size_of::<#atys>(), ::core::mem::size_of::<#htys>()),
                    ::ape::host::xnu_min(::core::mem::align_of::<#atys>(), ::core::mem::align_of::<#htys>()),
                )),*])
            };
        },
        quote! {
            #(const _: () = assert!(
                ::ape::host::xnu_min(::core::mem::size_of::<#atys>(), ::core::mem::size_of::<#htys>()) <= 8,
                #msgs
            );)*
        },
        // the registers as the host left them, and where its stack
        // arguments begin
        quote! {
            #[allow(clippy::too_many_arguments, unused_variables, unused_mut)]
            extern "C" fn xnu_inner(
                __ape_x0: u64, __ape_x1: u64, __ape_x2: u64, __ape_x3: u64,
                __ape_x4: u64, __ape_x5: u64, __ape_x6: u64, __ape_x7: u64,
                __ape_d0: f64, __ape_d1: f64, __ape_d2: f64, __ape_d3: f64,
                __ape_d4: f64, __ape_d5: f64, __ape_d6: f64, __ape_d7: f64,
                __ape_stack: *const u8,
            ) #hret {
                use ::ape::host::XnuKind as _;
                let mut __ape_a = ::ape::host::XnuArgs::new(
                    [__ape_x0, __ape_x1, __ape_x2, __ape_x3, __ape_x4, __ape_x5, __ape_x6, __ape_x7],
                    [__ape_d0, __ape_d1, __ape_d2, __ape_d3, __ape_d4, __ape_d5, __ape_d6, __ape_d7],
                    __ape_stack,
                );
                #(let #anames: #atys = unsafe { __ape_a.next::<#atys, #htys>(::ape::host::Kind::<#htys>::FLOAT) };)*
                let __ape_r = super::#name(#(#anames),*);
                #ret
            }
        },
        quote! {
            #[unsafe(naked)]
            pub unsafe extern "C" fn xnu_packed() {
                ::core::arch::naked_asm!(
                    "sub sp, sp, #32",
                    "stp x29, x30, [sp, #16]",
                    "add x29, sp, #16",
                    "add x9, sp, #32",
                    "str x9, [sp]",
                    "bl {inner}",
                    "ldp x29, x30, [sp, #16]",
                    "add sp, sp, #32",
                    "ret",
                    inner = sym xnu_inner,
                )
            }
        },
        quote! {
            pub fn xnu_entry() -> *const ::core::ffi::c_void {
                if __APE_PACKS {
                    xnu_packed as *const ::core::ffi::c_void
                } else {
                    xnu as *const ::core::ffi::c_void
                }
            }
        },
    ]
}

/// For a signature no Rust function can have (a `long double` by value on
/// Linux, a structure laid out differently by value) the host gets a libffi
/// closure, made the first time the entry point is asked for.
fn ffi_module(d: &Decl, vis: &syn::Visibility, full: &Full) -> TokenStream {
    let name = &d.name;
    let handlers = [
        ("sysv", &full.sysv),
        ("xnu", &full.xnu),
        ("win64", &full.win64),
    ]
    .map(|(conv, h)| handler(d, h, conv));
    quote! {
        #[allow(non_snake_case)]
        #vis mod #name {
            #[allow(unused_imports)]
            use super::*;
            #(#handlers)*
            /// The entry point to give a host library on this host.
            pub fn ptr() -> ::ape::host::Callback {
                static __APE_CLOSURE: ::std::sync::OnceLock<::ape::host::ffi::Closure> = ::std::sync::OnceLock::new();
                let c = __APE_CLOSURE.get_or_init(|| {
                    #[cfg(target_arch = "x86_64")]
                    if ::ape::is_windows() {
                        return win64::closure();
                    }
                    #[cfg(target_arch = "aarch64")]
                    if ::ape::is_xnu() {
                        return xnu::closure();
                    }
                    sysv::closure()
                });
                ::ape::host::Callback(c.code())
            }
        }
    }
}

fn handler(d: &Decl, h: &Sig, conv: &str) -> TokenStream {
    let conv = Ident::new(conv, Span::call_site());
    let name = &d.name;
    let anames = d.names();
    let atys = d.types();
    let htys: Vec<&Type> = h.args.iter().map(|(t, _)| t).collect();
    let hret_paren = paren(&h.ret);
    let r = Ident::new("__ape_r", Span::call_site());
    let ret = to_host(d, h, &r);
    quote! {
        pub mod #conv {
            #[allow(unused_imports)]
            use super::*;
            pub fn closure() -> ::ape::host::ffi::Closure {
                ::ape::host::ffi::Closure::new(
                    &[#(<#htys as ::ape::host::ffi::FfiKind>::kind()),*],
                    <#hret_paren as ::ape::host::ffi::FfiKind>::kind(),
                    handler,
                    ::core::ptr::null_mut(),
                )
            }
            #[allow(unused_variables, unused_mut, unused_assignments)]
            unsafe extern "C" fn handler(
                _cif: *mut u8,
                __ape_ret: *mut ::core::ffi::c_void,
                __ape_args: *mut *mut ::core::ffi::c_void,
                _user: *mut ::core::ffi::c_void,
            ) {
                let mut __ape_i = 0usize;
                #(let #anames: #atys = unsafe {
                    let __ape_p = *__ape_args.add(__ape_i) as *const #htys;
                    __ape_i += 1;
                    <#atys as ::ape::host::layout::HostRet<#htys>>::from_host(__ape_p.read_unaligned())
                };)*
                let __ape_r = super::super::#name(#(#anames),*);
                let __ape_r: #hret_paren = #ret;
                unsafe { ::ape::host::ffi::closure_ret(__ape_ret, __ape_r) };
            }
        }
    }
}
