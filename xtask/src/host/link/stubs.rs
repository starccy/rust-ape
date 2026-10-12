//! The archive of stubs for one library: its assembly and its cache key.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

use super::exports::exports_of;
use super::find::lib_symbol;
use super::sdk::{is_tbd, sdk_exports};
use super::{LINKGEN_HASH, OSES, Options, host_names};
use crate::util;

pub(super) fn stub_archive(args: &Options, name: &str, path: &Path) -> Result<PathBuf> {
    let meta = fs::metadata(path)?;
    let mtime = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let overrides = std::env::var("RUST_APE_HOST_LIBS").unwrap_or_default();
    let mut h = Sha256::new();
    h.update(format!(
        "{LINKGEN_HASH}\0{}\0{name}\0{}\0{}\0{mtime}\0{overrides}",
        args.arch,
        path.display(),
        meta.len()
    ));
    let key: String = h
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    let archive = args.out_dir.join(format!("{name}-{key}-{}.a", args.arch));
    if archive.is_file() {
        return Ok(archive);
    }

    let mut exports = exports_of(path)?;
    let soname = exports
        .soname
        .clone()
        .unwrap_or_else(|| path.file_name().unwrap().to_string_lossy().into_owned());
    // what macOS exports under the name too, when the SDK says, and the
    // name it's loaded by there
    let mut install = None;
    if !is_tbd(path)
        && let Some(sdk) = sdk_exports(name)?
    {
        install = sdk.soname;
        exports.functions.extend(sdk.functions);
        exports.functions.sort();
        exports.functions.dedup();
    }
    let asm = generate(
        &args.arch,
        name,
        &runtime_names(name, &soname, is_tbd(path), install.as_deref())?,
        &exports.functions,
        &exports.data,
    )?;

    // several links may want the same archive at once, each writes its own
    // and the last rename wins
    let tmp = args.out_dir.join(format!(
        ".{name}-{key}-{}.{}",
        args.arch,
        std::process::id()
    ));
    let src = tmp.with_extension("S");
    let obj = tmp.with_extension("o");
    let ar = tmp.with_extension("a");
    fs::write(&src, asm)?;
    util::run(
        Command::new(&args.cc)
            .arg("-c")
            .arg("-o")
            .arg(&obj)
            .arg(&src),
    )?;
    let _ = fs::remove_file(&ar);
    util::run(Command::new(&args.ar).arg("rcs").arg(&ar).arg(&obj))?;
    fs::rename(&ar, &archive)?;
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&obj);
    Ok(archive)
}

// `tbd` says the library was found as a macOS stub, whose install name
// is `soname`: it's for macOS alone unless the metadata says otherwise.
// `install` is the install name the SDK gives the library otherwise
fn runtime_names(
    name: &str,
    soname: &str,
    tbd: bool,
    install: Option<&str>,
) -> Result<[Vec<String>; 3]> {
    let mut names = if tbd {
        [Vec::new(), vec![soname.to_string()], Vec::new()]
    } else {
        [
            vec![soname.to_string()],
            vec![install.map_or_else(|| format!("lib{name}.dylib"), String::from)],
            vec![
                format!("{name}.dll"),
                format!("lib{name}.dll"),
                format!("lib{name}-0.dll"),
            ],
        ]
    };
    let config = std::env::var("RUST_APE_HOST_LIBS").unwrap_or_default();
    if !config.is_empty() {
        let config: serde_json::Value = serde_json::from_str(&config)?;
        for (i, os) in OSES.iter().enumerate() {
            if let Some(list) = host_names(&config, name, os) {
                names[i] = list;
            }
        }
    }
    for n in names.iter().flatten() {
        if n.is_empty() || n.contains(['"', '\\', '\n']) {
            bail!("bad library name {n:?} for -l{name}");
        }
    }
    Ok(names)
}

fn generate(
    arch: &str,
    name: &str,
    runtime: &[Vec<String>; 3],
    functions: &[String],
    data: &[(String, u64)],
) -> Result<String> {
    let x86 = match arch {
        "x86_64" => true,
        "aarch64" => false,
        _ => bail!("unknown arch {arch}"),
    };
    let mut s = String::new();
    writeln!(s, "// stubs for -l{name}, made by `cargo xtask hostlink`")?;
    // struct HostLinkLib in libc/dlopen/hostlink.c
    writeln!(
        s,
        "\t.section .data.hostlink.lib,\"aw\",@progbits\n\t.balign 8"
    )?;
    let lib = lib_symbol(name);
    writeln!(s, "\t.globl {lib}\n{lib}:")?;
    // struct HostLinkLib: handle, names per host, name, sync, flags
    let nosync = std::env::var("RUST_APE_HOST_LIBS")
        .ok()
        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
        .and_then(|c| {
            c.pointer(&format!("/{name}/sync"))
                .and_then(|v| v.as_bool())
        })
        == Some(false);
    writeln!(
        s,
        ".Llib:\t.quad 0, .Lnames0, .Lnames1, .Lnames2, .Llibname, 0, {}",
        nosync as u8
    )?;
    // for cosmo_hostlink() to find it by name
    writeln!(
        s,
        "\t.section .sort.rodata.hostlink.2,\"a\",@progbits\n\t.balign 8\n\t.quad .Llib"
    )?;
    writeln!(s, "\t.section .rodata.hostlink.lib,\"a\",@progbits")?;
    writeln!(s, ".Llibname:\t.asciz \"{name}\"")?;
    for (i, list) in runtime.iter().enumerate() {
        writeln!(s, ".Lnames{i}:")?;
        for n in list {
            writeln!(s, "\t.asciz \"{n}\"")?;
        }
        writeln!(s, "\t.byte 0")?;
    }
    for (i, f) in functions.iter().enumerate() {
        writeln!(s, "\t.section .text.hostlink.{f},\"ax\",@progbits")?;
        if !x86 {
            writeln!(s, "\t.balign 4")?;
        }
        writeln!(s, "\t.weak {f}\n\t.type {f},@function\n{f}:")?;
        if x86 {
            writeln!(s, "\tjmp *.Lslot{i}(%rip)")?;
            writeln!(
                s,
                ".Llazy{i}:\n\tlea .Lsym{i}(%rip),%r11\n\tjmp __hostlink_lazy"
            )?;
        } else {
            writeln!(
                s,
                "\tadrp x16,.Lslot{i}\n\tldr x16,[x16,#:lo12:.Lslot{i}]\n\tbr x16"
            )?;
            writeln!(
                s,
                ".Llazy{i}:\n\tadrp x17,.Lsym{i}\n\tadd x17,x17,#:lo12:.Lsym{i}\n\tb __hostlink_lazy"
            )?;
        }
        writeln!(s, "\t.size {f},.-{f}")?;
        writeln!(
            s,
            "\t.section .data.hostlink.{f},\"aw\",@progbits\n\t.balign 8"
        )?;
        writeln!(s, ".Lslot{i}:\t.quad .Llazy{i}")?;
        // struct HostLinkSym
        writeln!(
            s,
            "\t.section .rodata.hostlink.{f},\"a\",@progbits\n\t.balign 8"
        )?;
        writeln!(s, ".Lsym{i}:\t.quad .Lslot{i}, .Llib, .Lname{i}")?;
        writeln!(s, ".Lname{i}:\t.asciz \"{f}\"")?;
    }
    // storage for the library's variables, which the program's code binds
    // to; a crate declaring them registers them with __hostlink_data()
    for (d, size) in data {
        let align = (1u64 << size.trailing_zeros().min(4)).max(1);
        writeln!(
            s,
            "\t.section .bss.hostlink.{d},\"aw\",@nobits\n\t.weak {d}\n\t.type {d},@object"
        )?;
        writeln!(
            s,
            "\t.balign {align}\n{d}:\t.zero {size}\n\t.size {d},{size}"
        )?;
    }
    Ok(s)
}
