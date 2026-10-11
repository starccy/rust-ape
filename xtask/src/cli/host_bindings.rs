use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::host::bind as core;

#[derive(Args)]
pub struct HostBindArgs {
    /// bindgen output, or any Rust file with `extern "C"` blocks
    input: PathBuf,
    /// Where to write the result (default: stdout)
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Name of the static that loads the library
    #[arg(long, default_value = "LIB")]
    name: String,
    /// Library names to try on Linux, in order (repeatable)
    #[arg(long)]
    linux: Vec<String>,
    /// Library names to try on macOS
    #[arg(long)]
    macos: Vec<String>,
    /// DLL names to try on Windows
    #[arg(long)]
    windows: Vec<String>,
    /// Library names to try on the BSDs
    #[arg(long)]
    bsd: Vec<String>,
    /// Leave function pointer types alone instead of making them
    /// `ape::host::Callback`
    #[arg(long)]
    keep_fn_types: bool,
    /// bindgen output for the same header made with
    /// `--target=x86_64-pc-windows-msvc`: structures and functions whose
    /// types differ there (`long`, `wchar_t`, `long double`) are made to
    /// follow the host at runtime, see ape::host::layout
    #[arg(long, value_name = "FILE")]
    windows_input: Option<PathBuf>,
    /// bindgen output for the same header made with
    /// `--target=aarch64-apple-macos`, where `long double` is a `double`
    #[arg(long, value_name = "FILE")]
    macos_input: Option<PathBuf>,
    /// With --windows-input: parameter `p` of function `f` points at as
    /// many `long`s or `wchar_t`s as parameter `n` says (repeatable)
    #[arg(long, value_name = "f:p=n")]
    len: Vec<String>,
    /// Go ahead without the per-host inputs a header using `long`,
    /// `wchar_t` or `long double` needs, and declare it as on Linux
    #[arg(long)]
    ignore_missing_hosts: bool,
}

pub fn run(args: &HostBindArgs) -> Result<()> {
    core::run(&core::Options {
        input: args.input.clone(),
        out: args.out.clone(),
        name: args.name.clone(),
        linux: args.linux.clone(),
        macos: args.macos.clone(),
        windows: args.windows.clone(),
        bsd: args.bsd.clone(),
        keep_fn_types: args.keep_fn_types,
        windows_input: args.windows_input.clone(),
        macos_input: args.macos_input.clone(),
        len: args.len.clone(),
        ignore_missing_hosts: args.ignore_missing_hosts,
    })
}
