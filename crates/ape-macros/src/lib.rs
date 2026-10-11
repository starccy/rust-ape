//! The attribute macros of the `ape` crate, which re-exports them as
//! `ape::host_library` and `ape::host_callback`. Nothing here is meant to
//! be used from this crate directly; the generated code names `::ape`.

use proc_macro::TokenStream;
use syn::parse_macro_input;

mod callback;
mod library;
mod spec;

/// Declares functions of a host C library.
///
/// ```ignore
/// /// zlib, as shipped by the host.
/// #[ape::host_library(pub static ZLIB = linux ["libz.so.1"], macos ["libz.1.dylib"],
///                     windows ["zlib1.dll"], bsd ["libz.so.6", "libz.so"])]
/// unsafe extern "C" {
///     pub fn zlibVersion() -> *const c_char;
///     pub fn crc32(crc: c_ulong, buf: *const u8, len: c_uint) -> c_ulong;
///     pub fn printf(fmt: *const c_char, ...) -> c_int;
/// }
/// ```
///
/// The attribute names the static that loads the library, here `ZLIB`, an
/// [`ape::host::LazyLibrary`](../ape/host/struct.LazyLibrary.html), and
/// the file names to try on each host, in order. Name only the hosts the
/// library exists on: `static OBJC = macos ["/usr/lib/libobjc.A.dylib"]`
/// loads on macOS and on no other host, where `load()` fails saying no
/// name was given. `static PY = global, linux [...]` loads with
/// `RTLD_GLOBAL`, which an interpreter needs so that the extension modules
/// it loads later can find its symbols. Doc comments on the block go on
/// the static.
///
/// Each function becomes an `unsafe fn` with the same signature. The library
/// loads when one of them is first called (or when `ZLIB.load()` is), and
/// each looks itself up once. A missing library or symbol panics with the
/// loader's message; call `load()` first to handle that gracefully. A
/// `#[link_name = "..."]` attribute names the symbol to look up. A
/// variadic function becomes a module with `sysv()` and `win64()`
/// returning the function pointer, see `ape::host_variadic!`. A `static`
/// becomes a function returning a pointer to the library's variable.
///
/// The block takes what bindgen prints. Where a host takes other types
/// than the declaration says, a `#[host(..)]` attribute gives them per
/// convention, and the arguments and return value are converted on the way
/// through by `ape::host::layout::HostArg` and `HostRet`:
///
/// ```ignore
/// #[host(sysv = fn(i32) -> i64, xnu = fn(i32) -> i64, win64 = fn(i32) -> i32)]
/// pub fn f(c: c_char) -> c_long;
/// ```
///
/// `sysv` is for Linux and the BSDs, `xnu` for macOS and `win64` for
/// Windows; one left out takes the declared signature, so `#[host(win64 =
/// fn(i32) -> i32)]` is enough for a `long` that only Windows sees
/// differently. This is where a `long` (32 bits on Windows), a `wchar_t` (16
/// there) and a `long double` (a `double` on Windows and on macOS arm64)
/// get converted, and where an integer smaller than 32 bits is widened,
/// which Apple's arm64 convention expects of the caller. A pointer type
/// may be followed by `[n]`, `n` being the parameter that says how many
/// elements it points at. `ffi` (`#[host(ffi, sysv = ..)]`) makes the call
/// go through libffi, for a `long double` or a structure laid out
/// differently passed by value, see `ape::host::ffi`. `cargo xtask
/// host-bindings` writes all of this from bindgen's output for each host.
/// What it can't convert it marks `#[host(unsupported = "why")]`, which
/// doesn't compile until the line is taken off.
#[proc_macro_attribute]
pub fn host_library(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as library::Args);
    let block = parse_macro_input!(item as syn::ItemForeignMod);
    library::expand(args, block).into()
}

/// Defines a function a host library can call back.
///
/// ```ignore
/// #[ape::host_callback]
/// fn compare(a: *const c_void, b: *const c_void) -> c_int {
///     unsafe { *(a as *const i32) - *(b as *const i32) }
/// }
/// // hand the library compare::ptr()
/// ```
///
/// This makes `compare` an `extern "C" fn` with the body, plus a module
/// `compare` whose `ptr()` returns the entry point for the host we're on:
/// the function itself, or on Windows a copy of it that takes Microsoft's
/// convention. Pass the library `compare::ptr()`, typed as
/// `ape::host::Callback` in the declaration.
///
/// Where a host passes other types than the declared ones, the attribute
/// says which, as `#[host(..)]` does on a `host_library` declaration:
/// `#[ape::host_callback(sysv = fn(..) -> .., xnu = .., win64 = ..)]`.
/// That is a `long` on Windows, a `long double` there and on macOS, an
/// integer smaller than 32 bits, which Apple's arm64 expects a callee to
/// return widened. Name only the conventions that differ,
/// `#[ape::host_callback(win64 = fn(i32) -> i32)]` say; the others take
/// the declared signature. With `ffi`, `#[ape::host_callback(ffi, sysv =
/// .., ..)]`, the host gets a libffi closure instead of a function, for a
/// signature no Rust function can have: a `long double` by value on
/// Linux, a structure laid out differently by value. The generator says
/// what to put here.
///
/// A callback with more than eight arguments gets, on macOS, an entry
/// that lays out the ones Apple's arm64 packs on the stack, when the
/// types say it packs any.
#[proc_macro_attribute]
pub fn host_callback(args: TokenStream, item: TokenStream) -> TokenStream {
    let spec = parse_macro_input!(args as spec::Spec);
    let f = parse_macro_input!(item as syn::ItemFn);
    match callback::expand(spec, f) {
        Ok(t) => t.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
