//! The Rust that replaces a declaration: a wrapper, a jump, a variable's
//! registration or a variadic value.

use std::fmt::Write as _;

use super::source::tokens;
use super::types::Signature;

// the wrapper keeps the block's ABI, so it still coerces to the crate's
// fn pointer types ("C-unwind" is its own type)
pub(super) fn wrapper(
    f: &syn::ForeignItemFn,
    abi: &str,
    sym: &str,
    lib: &str,
    sync: bool,
    model: &Signature,
) -> String {
    let mut attrs: String = f
        .attrs
        .iter()
        .filter(|a| !a.path().is_ident("link_name"))
        .map(|a| tokens(a) + "\n")
        .collect();
    let vis = tokens(&f.vis);
    let name = &f.sig.ident;
    // a pointer to a `long` is read at the DLL's width, which a caller
    // gets told
    if let Some((p, c)) = model.pointers.first() {
        let note = format!(
            "rust-ape: `{p}` points at a C `{c}`, which a Windows DLL reads {} bytes wide; a \
binding made for the host's data model converts it, see README.md, the host data model",
            if *c == "wchar_t" { 2 } else { 4 }
        );
        let _ = writeln!(attrs, "#[deprecated(note = {note:?})]");
    }
    let tys: Vec<String> = f
        .sig
        .inputs
        .iter()
        .map(|a| match a {
            syn::FnArg::Typed(t) => tokens(&t.ty),
            syn::FnArg::Receiver(_) => unreachable!(),
        })
        .collect();
    // a `char` is signed on Apple's arm64, where the caller extends it
    let chars: Vec<bool> = f
        .sig
        .inputs
        .iter()
        .map(|a| match a {
            syn::FnArg::Typed(t) => match &*t.ty {
                syn::Type::Path(p) => p.path.segments.last().is_some_and(|s| s.ident == "c_char"),
                _ => false,
            },
            syn::FnArg::Receiver(_) => unreachable!(),
        })
        .collect();
    let params: String = tys
        .iter()
        .enumerate()
        .map(|(i, t)| format!("a{i}: {t}, "))
        .collect();
    let ty_list = tys.join(", ");
    let ret = tokens(&f.sig.output);
    let host_args: String = (0..tys.len())
        .map(|i| match model.callbacks.iter().find(|(j, _)| *j == i) {
            Some((_, m)) => format!("crate::__ape_hostlink::host_arg_m::<_, {m:#x}>(a{i}), "),
            None => format!("crate::__ape_hostlink::host_arg(a{i}), "),
        })
        .collect();
    let nargs = tys.len();
    // integers narrower than 32 bits go to the host as 32-bit ones, the
    // types of the call filled in from them
    let holes: String = (0..nargs).map(|_| "_, ").collect();
    let wide_args: String = (0..nargs)
        .map(|i| {
            if chars[i] {
                format!("crate::__ape_hostlink::Wide(a{i} as i8).wide(), ")
            } else {
                format!("crate::__ape_hostlink::Wide(a{i}).wide(), ")
            }
        })
        .collect();
    // a library with variables has them synced around the call where the
    // host keeps its own copies
    let (before, mut after) = if sync {
        (
            format!("let s = crate::__ape_hostlink::sync_in({lib});"),
            format!("if s {{ crate::__ape_hostlink::sync_out({lib}); }}"),
        )
    } else {
        (String::new(), String::new())
    };
    // a `long` comes back from a DLL in the low 32 bits, a `wchar_t` in
    // the low 16, the rest is whatever the register held
    let narrow = match model.ret {
        Some("long") => "i32",
        Some("unsigned long") => "u32",
        Some("wchar_t") => "u16",
        _ => "",
    };
    if !narrow.is_empty() {
        let ret_ty = match &f.sig.output {
            syn::ReturnType::Type(_, t) => tokens(t),
            syn::ReturnType::Default => unreachable!(),
        };
        let _ = write!(
            after,
            " let r = if crate::__ape_hostlink::is_windows() {{ (r as {narrow}) as {ret_ty} }} else {{ r }};"
        );
    }
    // also under the C name, weakly, for C code in the program calling it
    format!(
        r#"
{attrs}#[allow(warnings, clippy::all)]
#[unsafe(export_name = "{sym}")]
#[linkage = "weak"]
{vis} unsafe extern "{abi}" fn {name}({params}) {ret} {{
    static SLOT: crate::__ape_hostlink::Slot = crate::__ape_hostlink::Slot::new();
    unsafe {{
        #[cfg(target_arch = "x86_64")]
        {{
            if crate::__ape_hostlink::is_windows() {{
                let f: unsafe extern "win64" fn({ty_list}) {ret} =
                    crate::__ape_hostlink::__core::mem::transmute(SLOT.direct({lib}, b"{sym}\0", {nargs}));
                {before}
                let r = f({host_args});
                {after}
                return r;
            }}
        }}
        use crate::__ape_hostlink::WideAny as _;
        let f: unsafe extern "C" fn({holes}) {ret} =
            crate::__ape_hostlink::__core::mem::transmute(SLOT.tramp({lib}, b"{sym}\0"));
        {before}
        let r = f({wide_args});
        {after}
        r
    }}
}}
"#
    )
}

// A declaration with no parameters and no return type is only ever called
// through a pointer with the real signature (objc2 takes `objc_msgSend`
// that way), so it becomes a jump that leaves every register and the
// stack alone, bound the first time through `__hostlink_lazy` like a stub
// of the archive
pub(super) fn jump(f: &syn::ForeignItemFn, abi: &str, sym: &str, lib: &str) -> String {
    let attrs: String = f
        .attrs
        .iter()
        .filter(|a| !a.path().is_ident("link_name"))
        .map(|a| tokens(a) + "\n")
        .collect();
    let cfgs: String = f
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .map(|a| tokens(a) + "\n")
        .collect();
    let vis = tokens(&f.vis);
    let name = &f.sig.ident;
    let head = format!(
        r#"
{attrs}#[allow(warnings, clippy::all)]
#[unsafe(export_name = "{sym}")]
#[linkage = "weak"]
#[unsafe(naked)]
"#
    );
    format!(
        r#"
{cfgs}#[allow(warnings, clippy::all)]
static __APE_JUMP_{name}: crate::__ape_hostlink::__core::sync::atomic::AtomicPtr<crate::__ape_hostlink::__core::ffi::c_void> =
    crate::__ape_hostlink::__core::sync::atomic::AtomicPtr::new(0 as *mut crate::__ape_hostlink::__core::ffi::c_void);
{cfgs}#[allow(warnings, clippy::all)]
static __APE_SYM_{name}: crate::__ape_hostlink::Sym =
    crate::__ape_hostlink::Sym::new(&__APE_JUMP_{name}, unsafe {{ ({lib}) as *const u8 }}, b"{sym}\0");
{head}#[cfg(target_arch = "x86_64")]
{vis} unsafe extern "{abi}" fn {name}() {{
    crate::__ape_hostlink::__core::arch::naked_asm!(
        "mov r11, qword ptr [rip + {{slot}}]",
        "test r11, r11",
        "jz 2f",
        "jmp r11",
        "2:",
        "lea r11, [rip + {{sym}}]",
        "jmp {{lazy}}",
        slot = sym __APE_JUMP_{name},
        sym = sym __APE_SYM_{name},
        lazy = sym crate::__ape_hostlink::__hostlink_lazy,
    )
}}
{head}#[cfg(target_arch = "aarch64")]
{vis} unsafe extern "{abi}" fn {name}() {{
    crate::__ape_hostlink::__core::arch::naked_asm!(
        "adrp x16, {{slot}}",
        "ldr x16, [x16, :lo12:{{slot}}]",
        "cbz x16, 2f",
        "br x16",
        "2:",
        "adrp x17, {{sym}}",
        "add x17, x17, :lo12:{{sym}}",
        "b {{lazy}}",
        slot = sym __APE_JUMP_{name},
        sym = sym __APE_SYM_{name},
        lazy = sym crate::__ape_hostlink::__hostlink_lazy,
    )
}}
"#
    )
}

// A variable stays declared as it is, bound to the storage the library's
// archive of stubs has for it, and gets registered at startup so the
// library shares it or has it copied over when it's loaded
pub(super) fn register(st: &syn::ForeignItemStatic, sym: &str, libsym: &str, size: u64) -> String {
    let cfgs: String = st
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .map(|a| tokens(a) + "\n")
        .collect();
    let name = &st.ident;
    let r = if matches!(st.mutability, syn::StaticMutability::Mut(_)) {
        "mut"
    } else {
        "const"
    };
    format!(
        r#"
{cfgs}#[used]
#[doc(hidden)]
#[allow(warnings, clippy::all)]
#[unsafe(link_section = ".init_array")]
static __APE_DATA_{name}: unsafe extern "C" fn() = {{
    unsafe extern "C" fn register() {{
        unsafe {{
            crate::__ape_hostlink::data(&raw const crate::__ape_hostlink::{libsym}, b"{sym}\0", &raw {r} {name} as *mut u8, {size});
        }}
    }}
    register
}};
"#
    )
}

// A variadic function becomes a static of a type that implements the Fn
// traits for every number of variadic arguments, each of them generic, so a
// call keeps its syntax and every call site hands over its arguments' types.
// The static is declared in an extern block so that using it takes
// `unsafe`, as calling the function did
pub(super) fn variadic(f: &syn::ForeignItemFn, sym: &str, lib: &str, tag: &str) -> Option<String> {
    let tys: Vec<String> = f
        .sig
        .inputs
        .iter()
        .map(|a| match a {
            syn::FnArg::Typed(t) => tokens(&t.ty),
            syn::FnArg::Receiver(_) => unreachable!(),
        })
        .collect();
    // a reference would need a lifetime on the impl, and `!` isn't a type yet
    if tys.iter().any(|t| t.contains('&'))
        || matches!(&f.sig.output, syn::ReturnType::Type(_, t) if matches!(**t, syn::Type::Never(_)))
    {
        return None;
    }
    let attrs: String = f
        .attrs
        .iter()
        .filter(|a| !a.path().is_ident("link_name"))
        .map(|a| tokens(a) + "\n")
        .collect();
    let cfgs: String = f
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg"))
        .map(|a| tokens(a) + "\n")
        .collect();
    let vis = tokens(&f.vis);
    let name = &f.sig.ident;
    let ret = match &f.sig.output {
        syn::ReturnType::Default => "()".to_string(),
        syn::ReturnType::Type(_, t) => tokens(t),
    };
    let fixed: Vec<String> = tys
        .iter()
        .enumerate()
        .map(|(i, t)| format!("a{i}: {t}"))
        .collect();
    let fixed = fixed.join(", ");
    // the type's name is what an error names when the value is taken for
    // a function
    let ty = format!("{name}_is_variadic_and_has_no_function_pointer");
    Some(format!(
        r#"
/// The variadic function `{name}` of the host library, callable as it, and
/// only that: rust-ape lays each call's arguments out for the host from
/// their types, so there is no function pointer to it. See README.md,
/// C libraries on the host.
#[allow(warnings, clippy::all)]
#[repr(C)]
{cfgs}pub struct {ty};
#[allow(warnings, clippy::all)]
#[export_name = "__ape_variadic_{tag}_{name}"]
{cfgs}static __APE_VALUE_{name}: {ty} = {ty};
#[allow(warnings, clippy::all)]
{cfgs}unsafe extern "C" {{
    {attrs}#[link_name = "__ape_variadic_{tag}_{name}"]
    {vis} static {name}: {ty};
}}
#[allow(warnings, clippy::all)]
{cfgs}static __APE_SLOT_{name}: crate::__ape_hostlink::Slot = crate::__ape_hostlink::Slot::new();
{cfgs}crate::__ape_hostlink_variadic!({ty}, __APE_SLOT_{name}, {lib}, b"{sym}\0", [{fixed}], {ret});
"#
    ))
}
