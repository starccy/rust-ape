//! The RUSTC_WRAPPER `cargo xtask build` puts in front of rustc.
//!
//! Most crates go straight through. A crate that declares functions of a
//! library the program can only get from the host (the same libraries the
//! linker wrapper makes stubs for, see link/) is compiled from a
//! rewritten copy instead, in which every such declaration is a Rust
//! function with the same signature. The function looks the real one up
//! the first time it's called and calls it with the host's convention,
//! which on Windows is Microsoft's, so floating point and struct arguments
//! end up where the library expects them, and elsewhere passes an integer
//! narrower than 32 bits as a 32-bit one, which Apple's arm64 has the
//! caller extend it to. Callers don't change. It's also exported under the
//! C name, weakly, for C code in the program. A declaration with no
//! parameters and no return type is one a crate only takes the address of
//! (objc2's `objc_msgSend`), so it becomes a jump that leaves every
//! register alone, bound on first use like the archive's stubs. A callback
//! argument is made callable the host's way. A variadic function becomes a
//! static that implements the Fn traits, so each call site hands over its
//! arguments' types, and libc's `__hostlink_vcall()` lays them out for the
//! host. A variable stays declared and gets registered at startup, see
//! `__hostlink_data()`.
//!
//! What the rewrite can't make right it makes a compile-time matter, as
//! long as one of the crate's libraries can be loaded on Windows (the
//! host that differs; `windows = []` in `host-libs` says it can't). A
//! function pointer in a struct field is typed `HostCallback<F>` in the
//! copy, so assigning a plain function is a type error and
//! `HostCallback::new(f)` is what makes it callable by the DLL. A struct
//! with a `long`, `unsigned long` or `wchar_t` field, or an alias of one,
//! and a function taking a pointer to such a type, carry a `deprecated`
//! note saying that a Windows DLL sees another layout. A `long` returned
//! by value is widened from the 32 bits the DLL gives. See `Types`.
//!
//! The copy lives under the target directory. Only the files that change
//! are written; everything else in the crate's directory (and its OUT_DIR,
//! where bindgen output usually is) is a symlink, so relative paths and
//! `include!`s keep working. If the rewritten crate fails to compile, the
//! original is compiled instead and the stubs cover it.
//!
//! The windows-sys and windows crates, and any crate whose Cargo.toml says
//! `windows-crate = true` under `[package.metadata.rust-ape]` or that the
//! project lists under `windows-crates`, are Windows code: their
//! `extern "system"` means Microsoft's convention, so in the x86-64 half
//! it becomes `extern "win64"`, which gives COM vtables, callbacks and
//! every function pointer type the right convention with no thunk in
//! between. Their `link!("x.dll" "system" fn ..)` declarations become
//! functions that look the name up in that DLL when first called, and
//! `cfg(windows)` holds in them. Their proc macros, which write the same
//! `extern "system"` into other crates, are compiled from a copy that
//! replaces it in their output, see `windows_macro()`. Every call a
//! wrapper makes in Microsoft's convention goes through a stub outside the
//! program (`cosmo_dlhostcall()`), so that a host function tail-calling a
//! thunk it was handed doesn't leave it a return address of ours. A crate
//! listed under `apple-crates` is
//! macOS code the same way: `cfg(target_os = "macos")` holds. A framework
//! a crate links, which rustc won't take for this target, is dropped from
//! the link and its functions look themselves up in the framework, told
//! apart by the SDK's stub libraries when RUST_APE_MACOS_SDK names one.

mod codegen;
mod crate_info;
mod host_edits;
mod plan;
mod runtime;
mod shadow;
mod source;
mod transform;
mod types;
mod windows_macro;

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use self::transform::transform;
use self::windows_macro::windows_macro;

/// Set by `cargo xtask build` along with RUSTC_WRAPPER pointing at xtask
pub const ENV: &str = "RUST_APE_RUSTC_WRAP";

/// A wrapper the user had set, which we run rustc through in turn
pub const NEXT: &str = "RUST_APE_RUSTC_WRAPPER_NEXT";

/// Bumped when the rewrite changes, which rebuilds everything once
pub const VERSION: u32 = 13;

/// `args` is what cargo passed: rustc's path, then its arguments.
pub fn run(args: Vec<OsString>) -> i32 {
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let result = match windows_macro(&args) {
        Ok(None) => transform(&args),
        r => r,
    };
    match result {
        Ok(Some(code)) => code,
        Ok(None) => exec(&args, &[]),
        Err(e) => {
            eprintln!("warning: rust-ape: compiling without host library wrappers: {e:#}");
            exec(&args, &[])
        }
    }
}

// rustc refuses `-l framework=X` for a target that isn't Apple's; the
// wrapper's declarations for the framework's functions replace the link
fn without_frameworks(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-l" {
            match it.next() {
                Some(v) if v.starts_with("framework=") => continue,
                Some(v) => {
                    out.push(a.clone());
                    out.push(v.clone());
                }
                None => out.push(a.clone()),
            }
        } else if a.starts_with("-lframework=") {
            continue;
        } else {
            out.push(a.clone());
        }
    }
    out
}

pub(super) fn command(args: &[String], env: &[(&str, &Path)]) -> Command {
    let args = without_frameworks(args);
    let args = &args;
    let mut cmd = match std::env::var_os(NEXT).filter(|w| !w.is_empty()) {
        Some(next) => {
            let mut c = Command::new(next);
            c.args(args);
            c
        }
        None => {
            let mut c = Command::new(&args[0]);
            c.args(&args[1..]);
            c
        }
    };
    for (k, v) in env {
        cmd.env(k, v);
    }
    // read by the windows-interface copy, see windows_macro()
    if let Some(abi) = host_abi(args) {
        cmd.env(HOST_ABI, abi);
    }
    cmd
}

/// Set on every target unit's rustc, for the proc macros to read
const HOST_ABI: &str = "RUST_APE_HOST_ABI";

// the ABI Windows code gets in the half of the image for the target
pub(super) fn host_abi(args: &[String]) -> Option<&'static str> {
    let target = values(args, "--target").into_iter().next()?;
    if !target.ends_with(".json") || !target.contains("/generated/") {
        return None;
    }
    let file = Path::new(target).file_name()?.to_str()?;
    Some(if file.starts_with("x86_64-") {
        "win64"
    } else {
        "C"
    })
}

pub(super) fn exec(args: &[String], env: &[(&str, &Path)]) -> i32 {
    match command(args, env).status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("error: rust-ape: could not run {}: {e}", args[0]);
            1
        }
    }
}

/// The value after `flag`, given as `flag value` or `flagvalue`
pub(super) fn values<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            if let Some(v) = it.next() {
                out.push(v.as_str());
            }
        } else if let Some(v) = a
            .strip_prefix(flag)
            .filter(|v| !v.is_empty() && flag.len() == 2)
        {
            out.push(v);
        } else if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
            out.push(v);
        }
    }
    out
}
