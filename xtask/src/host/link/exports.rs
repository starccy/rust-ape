//! The symbols a shared object exports, read from ELF, PE or Mach-O.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::LINKGEN_HASH;
use super::sdk::{is_tbd, tbd_exports};
use crate::util;

#[derive(Clone, Default)]
pub struct Exports {
    pub soname: Option<String>,
    pub functions: Vec<String>,
    /// variables, with their sizes
    pub data: Vec<(String, u64)>,
}

/// What the shared object at `path` exports, cached under
/// generated/hostlink since reading a big library's symbols takes a while
/// and every crate that links it asks
pub fn exports_of(path: &Path) -> Result<Exports> {
    let meta = fs::metadata(path)?;
    let mtime = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let mut h = Sha256::new();
    h.update(format!(
        "{LINKGEN_HASH}\0{}\0{}\0{mtime}",
        path.display(),
        meta.len()
    ));
    let key: String = h
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = util::repo_root().join("generated/hostlink");
    let cache = dir.join(format!(
        "{}-{key}.exports",
        path.file_name().unwrap().to_string_lossy()
    ));
    if let Ok(text) = fs::read_to_string(&cache) {
        let mut e = Exports::default();
        for line in text.lines() {
            match line.split_once(' ') {
                Some(("S", v)) => e.soname = Some(v.to_string()),
                Some(("F", v)) => e.functions.push(v.to_string()),
                Some(("D", v)) => {
                    if let Some((n, size)) = v.split_once(' ') {
                        e.data.push((n.to_string(), size.parse()?));
                    }
                }
                _ => {}
            }
        }
        return Ok(e);
    }
    let e = if is_tbd(path) {
        tbd_exports(path)
    } else {
        read_exports(&fs::read(path)?)
    }
    .with_context(|| format!("reading the symbols of {}", path.display()))?;
    let mut text = String::new();
    if let Some(so) = &e.soname {
        writeln!(text, "S {so}")?;
    }
    for f in &e.functions {
        writeln!(text, "F {f}")?;
    }
    for (d, size) in &e.data {
        writeln!(text, "D {d} {size}")?;
    }
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(".{key}.{}", std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &cache)?;
    Ok(e)
}

// ELF64 little endian only; the build machine is x86_64 or aarch64 Linux
fn read_exports(d: &[u8]) -> Result<Exports> {
    let u16_at = |o: usize| -> Result<u16> {
        Ok(u16::from_le_bytes(
            d.get(o..o + 2).context("truncated")?.try_into()?,
        ))
    };
    let u32_at = |o: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(
            d.get(o..o + 4).context("truncated")?.try_into()?,
        ))
    };
    let u64_at = |o: usize| -> Result<u64> {
        Ok(u64::from_le_bytes(
            d.get(o..o + 8).context("truncated")?.try_into()?,
        ))
    };
    if d.len() < 64 || &d[..4] != b"\x7fELF" || d[4] != 2 || d[5] != 1 {
        bail!("not a 64-bit little endian ELF file");
    }
    let shoff = u64_at(0x28)? as usize;
    let shentsize = u16_at(0x3a)? as usize;
    let shnum = u16_at(0x3c)? as usize;
    struct Sh {
        kind: u32,
        off: usize,
        size: usize,
        link: usize,
        entsize: usize,
    }
    let mut shs = Vec::with_capacity(shnum);
    for i in 0..shnum {
        let b = shoff + i * shentsize;
        shs.push(Sh {
            kind: u32_at(b + 4)?,
            off: u64_at(b + 0x18)? as usize,
            size: u64_at(b + 0x20)? as usize,
            link: u32_at(b + 0x28)? as usize,
            entsize: u64_at(b + 0x38)? as usize,
        });
    }
    let cstr = |off: usize| -> Result<String> {
        let tail = d.get(off..).context("bad string offset")?;
        let end = tail
            .iter()
            .position(|&c| c == 0)
            .context("unterminated string")?;
        Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
    };

    const SHT_DYNSYM: u32 = 11;
    const SHT_DYNAMIC: u32 = 6;
    const SHT_GNU_VERSYM: u32 = 0x6fff_ffff;
    const DT_SONAME: u64 = 14;

    let mut soname = None;
    if let Some(dy) = shs.iter().find(|s| s.kind == SHT_DYNAMIC) {
        let strtab = shs.get(dy.link).context("bad dynamic link")?;
        for e in (dy.off..dy.off + dy.size).step_by(16) {
            let tag = u64_at(e)?;
            if tag == 0 {
                break;
            }
            if tag == DT_SONAME {
                soname = Some(cstr(strtab.off + u64_at(e + 8)? as usize)?);
            }
        }
    }

    let sym = shs
        .iter()
        .find(|s| s.kind == SHT_DYNSYM)
        .context("no .dynsym")?;
    let strtab = shs.get(sym.link).context("bad dynsym link")?;
    let versym = shs.iter().find(|s| s.kind == SHT_GNU_VERSYM);
    let entsize = if sym.entsize == 0 { 24 } else { sym.entsize };
    let mut functions = Vec::new();
    let mut data = Vec::new();
    for i in 0..sym.size / entsize {
        let e = sym.off + i * entsize;
        let info = d[e + 4];
        let other = d[e + 5];
        let shndx = u16_at(e + 6)?;
        let (bind, kind) = (info >> 4, info & 15);
        // defined, global or weak, a function, an ifunc or a variable,
        // default or protected visibility (Qt 6 exports everything protected)
        if shndx == 0
            || !(bind == 1 || bind == 2)
            || !(kind == 1 || kind == 2 || kind == 10)
            || !matches!(other & 3, 0 | 3)
        {
            continue;
        }
        // a hidden version is an old one kept for old binaries
        if let Some(v) = versym
            && u16_at(v.off + i * 2)? & 0x8000 != 0
        {
            continue;
        }
        let name = cstr(strtab.off + u32_at(e)? as usize)?;
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            continue;
        }
        if kind == 1 {
            let size = u64_at(e + 16)?;
            if size > 0 {
                data.push((name, size));
            }
        } else {
            functions.push(name);
        }
    }
    functions.sort();
    functions.dedup();
    data.sort();
    data.dedup_by(|a, b| a.0 == b.0);
    Ok(Exports {
        soname,
        functions,
        data,
    })
}
