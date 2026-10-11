//! `#![windows_subsystem = "..."]` for APE binaries. rustc only acts on it
//! when the target is Windows, which ours never is, so the build reads it
//! from the crate root and writes it into the PE header of the finished
//! file instead.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use syn::punctuated::Punctuated;
use syn::{Expr, Lit, Meta, Token};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Subsystem {
    Windows,
    Console,
}

impl Subsystem {
    fn pe_value(self) -> u16 {
        match self {
            Subsystem::Windows => 2,
            Subsystem::Console => 3,
        }
    }
}

/// The subsystem the crate root at `src` asks for, if any. `debug` is
/// what `debug_assertions` evaluates to in a `cfg_attr`.
pub fn from_source(src: &Path, debug: bool) -> Result<Option<Subsystem>> {
    let text = fs::read_to_string(src).with_context(|| format!("reading {}", src.display()))?;
    let file = syn::parse_file(&text).with_context(|| format!("parsing {}", src.display()))?;
    let mut found = None;
    for attr in &file.attrs {
        if let syn::AttrStyle::Inner(_) = attr.style {
            scan(&attr.meta, debug, src, &mut found)?;
        }
    }
    Ok(found)
}

fn scan(meta: &Meta, debug: bool, src: &Path, found: &mut Option<Subsystem>) -> Result<()> {
    match meta {
        Meta::NameValue(nv) if nv.path.is_ident("windows_subsystem") => {
            let value = match &nv.value {
                Expr::Lit(e) => match &e.lit {
                    Lit::Str(s) => s.value(),
                    _ => String::new(),
                },
                _ => String::new(),
            };
            *found = Some(match value.as_str() {
                "windows" => Subsystem::Windows,
                "console" => Subsystem::Console,
                _ => bail!(
                    "{}: windows_subsystem must be \"windows\" or \"console\"",
                    src.display()
                ),
            });
        }
        Meta::List(list) if list.path.is_ident("cfg_attr") => {
            let args = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
            let mut args = args.iter();
            let Some(pred) = args.next() else {
                return Ok(());
            };
            match eval(pred, debug) {
                Some(true) => {
                    for m in args {
                        scan(m, debug, src, found)?;
                    }
                }
                Some(false) => {}
                None => {
                    if args.clone().any(|m| m.path().is_ident("windows_subsystem")) {
                        println!(
                            "warning: {}: can't evaluate the cfg_attr around windows_subsystem, ignoring it",
                            src.display()
                        );
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

// A predicate about Windows counts as true, since the header only matters
// when the binary runs there.
fn eval(pred: &Meta, debug: bool) -> Option<bool> {
    match pred {
        Meta::Path(p) if p.is_ident("debug_assertions") => Some(debug),
        Meta::Path(p) if p.is_ident("windows") => Some(true),
        Meta::Path(p) if p.is_ident("unix") => Some(false),
        Meta::NameValue(nv)
            if nv.path.is_ident("target_os") || nv.path.is_ident("target_family") =>
        {
            match &nv.value {
                Expr::Lit(e) => match &e.lit {
                    Lit::Str(s) => Some(s.value() == "windows"),
                    _ => None,
                },
                _ => None,
            }
        }
        Meta::List(l) => {
            let inner = l
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .ok()?;
            let vals: Option<Vec<bool>> = inner.iter().map(|m| eval(m, debug)).collect();
            let vals = vals?;
            if l.path.is_ident("not") && vals.len() == 1 {
                Some(!vals[0])
            } else if l.path.is_ident("all") {
                Some(vals.iter().all(|&v| v))
            } else if l.path.is_ident("any") {
                Some(vals.iter().any(|&v| v))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Sets the Subsystem field of the PE header at the start of `ape`.
pub fn apply(ape: &Path, subsystem: Subsystem) -> Result<()> {
    let mut f = OpenOptions::new().read(true).write(true).open(ape)?;
    let mut mz = [0u8; 64];
    f.read_exact(&mut mz)?;
    if &mz[..2] != b"MZ" {
        bail!("{}: no MZ header", ape.display());
    }
    let pe = u32::from_le_bytes(mz[0x3c..0x40].try_into().unwrap()) as u64;
    let mut hdr = [0u8; 24 + 70];
    f.seek(SeekFrom::Start(pe))?;
    f.read_exact(&mut hdr)?;
    if &hdr[..4] != b"PE\0\0" || u16::from_le_bytes([hdr[24], hdr[25]]) != 0x20b {
        bail!("{}: no PE32+ header at {pe:#x}", ape.display());
    }
    // OptionalHeader.Subsystem, 68 bytes into the optional header
    f.seek(SeekFrom::Start(pe + 24 + 68))?;
    f.write_all(&subsystem.pe_value().to_le_bytes())?;
    Ok(())
}
