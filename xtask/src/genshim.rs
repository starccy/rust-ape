//! regenerate shim/tables.h and the compile-time
//! cross-check example from the vendored sources.
//! Which constants go in is listed in shim/tables.toml.

use crate::util;
use anyhow::{Context, Result, bail};
use clap::Args;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct GenShimArgs {
    /// Print the tables instead of writing them
    #[arg(long)]
    pub dry_run: bool,
}

const ARCHES: &[&str] = &["x86_64", "aarch64"];

/// Search order inside the libc crate for one arch. most specific first.
pub(crate) fn libc_search_paths(root: &Path, arch: &str) -> Vec<PathBuf> {
    let src = root.join("vendor/patches/libc/src");
    [
        format!("unix/linux_like/linux/musl/b64/{arch}/mod.rs"),
        "unix/linux_like/linux/musl/b64/mod.rs".into(),
        "unix/linux_like/linux/musl/mod.rs".into(),
        "unix/linux_like/linux/arch/generic/mod.rs".into(),
        "unix/linux_like/linux/mod.rs".into(),
        "unix/linux_like/linux_l4re_shared.rs".into(),
        "unix/linux_like/mod.rs".into(),
        "unix/mod.rs".into(),
        // the libc crate's "new structure": FUTEX_* live here
        "new/linux_uapi/linux/futex.rs".into(),
    ]
    .into_iter()
    .map(|p| src.join(p))
    .collect()
}

/// One [[table]] of shim/tables.toml. `cosmo_header` is where each name has
/// to be declared, as a runtime symbol or as a #define (same value on every
/// platform, no translation needed, still asserted).
struct Domain {
    /// Name of the emitted X-macro, e.g. "SHIM_POLL_TABLE".
    macro_name: &'static str,
    cosmo_header: &'static str,
    /// (name, droppable) — droppable only matters for flag-style tables.
    names: &'static [(&'static str, bool)],
    with_droppable_column: bool,
    /// C type of the cosmo-side constant, for SHIM_FIX_* storage.
    ctype: &'static str,
}

/// The scraper reads every `pub const` without judging `cfg_if!` branches;
/// for the few names whose branches collide (the 32-bit time64 variants of
/// the SO_ timeouts), the 64-bit value is stated here explicitly. Wrong
/// entries cannot survive: the generated const asserts have rustc check
/// every value against the real cfg-resolved libc.
const CFG_OVERRIDES: &[(&str, i64, i64)] = &[
    // (name, x86_64, aarch64)
    ("SO_RCVTIMEO", 20, 20),
    ("SO_SNDTIMEO", 21, 21),
];

/// Names cosmo has dropped outright: no support on any host, so with no
/// table entry the shim passes the raw cmd through and cosmo answers
/// EINVAL, which is the accurate "not supported" behavior.
const OPTIONAL_IF_ABSENT: &[&str] = &["F_SETOWN", "F_GETOWN"];

/// libc crate name -> cosmo symbol name, where the two worlds disagree.
/// The C table carries the cosmo name (it takes the symbol's address); the
/// Rust assert uses the libc name.
pub(crate) fn cosmo_name(libc_name: &str) -> &str {
    match libc_name {
        "IPV6_ADD_MEMBERSHIP" => "IPV6_JOIN_GROUP",
        "IPV6_DROP_MEMBERSHIP" => "IPV6_LEAVE_GROUP",
        other => other,
    }
}

/// shim/tables.toml, the list of what gets translated.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    singles: Vec<String>,
    table: Vec<Table>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    r#macro: String,
    header: String,
    ctype: String,
    names: Vec<String>,
    /// present = the table has a droppable column
    droppable: Option<Vec<String>>,
}

fn load_manifest(root: &Path) -> Result<(Vec<&'static str>, Vec<Domain>)> {
    let path = root.join("shim/tables.toml");
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let m: Manifest = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    // since the manifest is read once and lives as long as the process,
    // so it's safe to leak its strings into &'static str for the code below.
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let mut domains = Vec::new();
    for t in m.table {
        let droppable = t.droppable.clone().unwrap_or_default();
        if let Some(stray) = droppable.iter().find(|d| !t.names.contains(d)) {
            bail!("{}: {stray} is droppable in {} but not one of its names", path.display(), t.r#macro);
        }
        let names: Vec<(&'static str, bool)> =
            t.names.into_iter().map(|n| { let d = droppable.contains(&n); (leak(n), d) }).collect();
        domains.push(Domain {
            macro_name: leak(t.r#macro),
            cosmo_header: leak(t.header),
            names: Box::leak(names.into_boxed_slice()),
            with_droppable_column: t.droppable.is_some(),
            ctype: leak(t.ctype),
        });
    }
    Ok((m.singles.into_iter().map(leak).collect(), domains))
}

/// Ask cosmocc's preprocessor what each cosmo-side name expands to. Names
/// that resolve to a plain integer are fixed portable values (`#define
/// TCP_NODELAY 1`): the table cannot take their address, so tables.h gives
/// them local storage instead. Names that stay identifiers are the extern
/// runtime constants.
fn classify_cosmo(root: &Path, names: &[(&str, &str)]) -> Result<HashMap<String, Option<i64>>> {
    let mut probe = String::from(
        "#include <errno.h>\n#include <fcntl.h>\n#include <poll.h>\n\
         #include <sys/socket.h>\n#include <sys/mman.h>\n\
         #include <libc/sysv/consts/at.h>\n#include <libc/sysv/consts/utime.h>\n\
         #include <libc/sysv/consts/sock.h>\n#include <libc/sysv/consts/so.h>\n\
         #include <libc/sysv/consts/sol.h>\n#include <libc/sysv/consts/tcp.h>\n\
         #include <libc/sysv/consts/ip.h>\n#include <libc/sysv/consts/ipv6.h>\n\
         #include <libc/sysv/consts/msg.h>\n#include <libc/sysv/consts/map.h>\n\
         #include <libc/sysv/consts/clock.h>\n#include <libc/sysv/consts/madv.h>\n\
         #include <libc/sysv/consts/sig.h>\n\
         #include <libc/sysv/consts/sa.h>\n#include <libc/sysv/consts/ss.h>\n\
         #include <libc/sysv/consts/auxv.h>\n\
         #include <libc/sysv/consts/af.h>\n\
         #include <libc/sysv/consts/termios.h>\n\
         #include <libc/sysv/consts/modem.h>\n\
         #include <libc/sysv/consts/st.h>\n\
         #include <libc/sysv/consts/iff.h>\n\
         #include <libc/sysv/consts/sched.h>\n\
         #include <libc/sysv/consts/baud.internal.h>\n\
         #include <libc/sysv/consts/fio.h>\n",
    );
    for (_, cname) in names {
        // A string literal survives expansion; the bare name after it is the
        // probe. cpp linemarkers shred line structure, so parsing joins
        // everything and splits on the quotes again.
        let _ = writeln!(probe, "\"{cname}\" {cname}");
    }
    
    let generated = root.join("generated");
    fs::create_dir_all(&generated)?;
    let probe_path = generated.join("genshim-probe.c");
    fs::write(&probe_path, probe)?;
    let out = util::capture(
        std::process::Command::new(root.join("vendor/cosmocc/bin/x86_64-unknown-cosmo-cc"))
            .arg("-E")
            .arg(&probe_path),
    )?;
    let _ = fs::remove_file(&probe_path);
    let joined: String = out
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ");
    let mut map = HashMap::new();
    let mut parts = joined.split('"');
    let _leading = parts.next();
    while let (Some(name), Some(expansion)) = (parts.next(), parts.next()) {
        let cleaned: String = expansion
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '(' && *c != ')')
            .collect();
        map.insert(name.to_string(), eval(&cleaned, &HashMap::new(), 7).ok());
    }
    for (_, cname) in names {
        if !map.contains_key(*cname) {
            bail!("preprocessor probe lost track of {cname}");
        }
    }
    Ok(map)
}

/// All `pub const NAME: ty = expr;` in one file, expression text unevaluated.
fn scrape_consts(path: &Path, out: &mut HashMap<String, String>) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    for line in text.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("pub const ") else { continue };
        let Some((name, rest)) = rest.split_once(':') else { continue };
        let Some((_ty, expr)) = rest.split_once('=') else { continue };
        let Some(expr) = expr.trim().strip_suffix(';') else { continue };
        // first definition wins: files are scanned most-specific first
        out.entry(name.trim().to_string()).or_insert_with(|| expr.trim().to_string());
    }
    Ok(())
}

/// Evaluate an extracted expression: integer literals (0x/0o/decimal, with _
/// separators and a possible `as` cast), identifiers (looked up recursively)
/// and `|` combinations thereof.
fn eval(expr: &str, consts: &HashMap<String, String>, depth: u32) -> Result<i64> {
    if depth > 8 {
        bail!("expression recurses too deep: {expr}");
    }
    let expr = expr.trim();
    if let Some((l, r)) = expr.split_once('|') {
        return Ok(eval(l, consts, depth + 1)? | eval(r, consts, depth + 1)?);
    }
    // libc wraps would-be-negative bit patterns: SA_RESETHAND = u32_cast_int(0x80000000).
    // Evaluate the inside, then reproduce the u32 -> i32 reinterpretation.
    if let Some(inner) = expr.strip_prefix("u32_cast_int(").and_then(|r| r.strip_suffix(')')) {
        return Ok(eval(inner, consts, depth + 1)? as u32 as i32 as i64);
    }
    // Same wrapper, ioctl flavor: musl's Ioctl is c_int, so TCGETS2 & co are
    // negative there.
    if let Some(inner) = expr.strip_prefix("u32_cast_ioctl(").and_then(|r| r.strip_suffix(')')) {
        return Ok(eval(inner, consts, depth + 1)? as u32 as i32 as i64);
    }
    let expr = expr.split(" as ").next().unwrap().trim();
    let expr = expr.trim_start_matches("crate::");
    let lit = expr.replace('_', "");
    let parsed = if let Some(h) = lit.strip_prefix("0x") {
        i64::from_str_radix(h, 16).ok()
    } else if let Some(o) = lit.strip_prefix("0o") {
        i64::from_str_radix(o, 8).ok()
    } else if lit.starts_with('0') && lit.len() > 1 && lit.chars().all(|c| c.is_ascii_digit()) {
        i64::from_str_radix(&lit[1..], 8).ok() // C-style octal shows up too
    } else {
        lit.parse::<i64>().ok()
    };
    if let Some(v) = parsed {
        return Ok(v);
    }
    let target = consts
        .get(expr)
        .with_context(|| format!("cannot resolve identifier {expr:?}"))?;
    eval(target, consts, depth + 1)
}

/// Verify a name is declared in the given cosmo header, as an extern runtime
/// constant or as a fixed #define (both are linkable/usable from the shim).
fn cosmo_declares(header_text: &str, name: &str) -> bool {
    header_text.lines().any(|l| {
        let t = l.trim();
        // last word, not a fixed position: the type may be multi-word
        // ("extern const unsigned long AT_MINSIGSTKSZ;")
        (t.starts_with("extern const") && t.split_whitespace().last().map(|w| w.trim_end_matches(';')) == Some(name))
            || t.starts_with(&format!("#define {name} "))
            || t.starts_with(&format!("#define {name}\t"))
    })
}

pub fn run(args: &GenShimArgs) -> Result<()> {
    let root = util::repo_root();
    let (singles, domains) = load_manifest(&root)?;

    // Scrape both arches' constant space once.
    let mut per_arch: HashMap<&str, HashMap<String, String>> = HashMap::new();
    for &arch in ARCHES {
        let mut consts = HashMap::new();
        for p in libc_search_paths(&root, arch) {
            scrape_consts(&p, &mut consts)?;
        }
        if consts.is_empty() {
            bail!("no constants scraped for {arch}; is vendor/patches/libc populated?");
        }
        per_arch.insert(arch, consts);
    }
    let value = |arch: &str, name: &str| -> Result<i64> {
        if let Some(&(_, x, a)) = CFG_OVERRIDES.iter().find(|(n, _, _)| *n == name) {
            return Ok(if arch == "x86_64" { x } else { a });
        }
        let consts = &per_arch[arch];
        let expr = consts
            .get(name)
            .with_context(|| format!("{name} not found in the libc crate for {arch}"))?;
        eval(expr, consts, 0).with_context(|| format!("evaluating {name} for {arch}"))
    };

    let mut h = String::new();
    let mut rs = String::new();
    let _ = writeln!(h, "/* Generated by `cargo xtask gen-shim`. DO NOT EDIT.");
    let _ = writeln!(h, " *");
    let _ = writeln!(h, " * Left column: the value musl bakes into the Rust world at compile");
    let _ = writeln!(h, " * time. Right side (taken by the shim as &NAME): cosmo's runtime");
    let _ = writeln!(h, " * constant for the same name. Cross-checked at build time by");
    let _ = writeln!(h, " * examples/src/bin/shim_tables_check.rs. */");
    let _ = writeln!(h, "#ifndef RUST_APE_SHIM_TABLES_H_");
    let _ = writeln!(h, "#define RUST_APE_SHIM_TABLES_H_");
    let _ = writeln!(rs, "//! Generated by `cargo xtask gen-shim`. DO NOT EDIT.");
    let _ = writeln!(rs, "//!");
    let _ = writeln!(rs, "//! One const assert per value in shim/tables.h: if the extraction ever");
    let _ = writeln!(rs, "//! disagrees with what rustc resolves libc's constants to, the build of");
    let _ = writeln!(rs, "//! either target fails right here instead of misbehaving at runtime.");
    let _ = writeln!(rs, "#![allow(overflowing_literals)]");
    let _ = writeln!(rs);

    let assert_line = |rs: &mut String, arch: &str, name: &str, v: i64| {
        let _ = writeln!(
            rs,
            "#[cfg(target_arch = \"{arch}\")] const _: () = assert!(libc::{name} as i64 == {v});"
        );
    };

    // SHIM_LIN_* singles first, then the tables.
    let _ = writeln!(h, "\n/* one-off Linux values the shim logic handles specially */");
    for &name in &singles {
        let x = value("x86_64", name)?;
        let a = value("aarch64", name)?;
        if x == a {
            let _ = writeln!(h, "#define SHIM_LIN_{name} {x}");
        } else {
            let _ = writeln!(h, "#if defined(__x86_64__)");
            let _ = writeln!(h, "#define SHIM_LIN_{name} {x}");
            let _ = writeln!(h, "#elif defined(__aarch64__)");
            let _ = writeln!(h, "#define SHIM_LIN_{name} {a}");
            let _ = writeln!(h, "#endif");
        }
        assert_line(&mut rs, "x86_64", name, x);
        assert_line(&mut rs, "aarch64", name, a);
    }

    // Resolve every name's cosmo-side spelling first: the canonical name, a
    // `_`-prefixed variant (newer cosmocc renamed O_PATH -> _O_PATH), or
    // absent (skippable for OPTIONAL_IF_ABSENT names and droppable ones). The
    // preprocessor probe must ask about the RESOLVED spelling, otherwise a
    // renamed macro classifies as a runtime symbol and the table would take
    // the address of something that no longer exists.
    let read_domain_header = |cosmo_header: &str| -> Result<String> {
        let header_path = root.join("vendor/cosmocc/include/libc").join(cosmo_header);
        fs::read_to_string(&header_path)
            .with_context(|| format!("could not read {}", header_path.display()))
    };
    let mut resolved: HashMap<(&str, &str), Option<String>> = HashMap::new();
    for d in &domains {
        let header = read_domain_header(d.cosmo_header)?;
        for &(name, droppable) in d.names {
            let base = cosmo_name(name);
            let underscored = format!("_{base}");
            let cname = if cosmo_declares(&header, base) {
                Some(base.to_string())
            } else if cosmo_declares(&header, &underscored) {
                println!("note: {base} is spelled {underscored} in this cosmocc");
                Some(underscored)
            } else if OPTIONAL_IF_ABSENT.contains(&base) || droppable {
                // droppable names are hints by definition; a cosmocc that
                // removed one simply doesn't support it anywhere.
                println!("note: {base} is gone from this cosmocc; pass-through/drop");
                None
            } else {
                bail!("{base} is not declared in cosmo's libc/{}", d.cosmo_header);
            };
            resolved.insert((d.macro_name, name), cname);
        }
    }
    let probe_names: Vec<(&str, String)> = domains
        .iter()
        .flat_map(|d| {
            d.names.iter().filter_map(|&(n, _)| {
                resolved[&(d.macro_name, n)].clone().map(|c| (n, c))
            })
        })
        .collect();
    let all_names: Vec<(&str, &str)> =
        probe_names.iter().map(|(n, c)| (*n, c.as_str())).collect();
    let fixedness = classify_cosmo(&root, &all_names)?;

    for d in &domains {
        let _ = writeln!(h, "\n/* {} <- libc crate; cosmo side declared in libc/{} */", d.macro_name, d.cosmo_header);
        // Arch-differing names become SHIM_LIN_<name> defines above the macro.
        let mut lines = Vec::new();
        for &(name, droppable) in d.names {
            let Some(cname) = resolved[&(d.macro_name, name)].as_deref() else {
                continue;
            };
            let x = value("x86_64", name)?;
            let a = value("aarch64", name)?;
            if x == a {
                let _ = writeln!(h, "#define SHIM_LIN_{name} {x}");
            } else {
                let _ = writeln!(h, "#if defined(__x86_64__)");
                let _ = writeln!(h, "#define SHIM_LIN_{name} {x}");
                let _ = writeln!(h, "#elif defined(__aarch64__)");
                let _ = writeln!(h, "#define SHIM_LIN_{name} {a}");
                let _ = writeln!(h, "#endif");
            }
            let cell = format!("SHIM_LIN_{name}");
            // Fixed portable values get addressable storage; the X user
            // always does &<first-arg>, so hand it the right identifier.
            let sym = match fixedness[cname] {
                Some(v) => {
                    let _ = writeln!(
                        h,
                        "static const {} SHIM_FIX_{cname} = {v}; /* cosmo fixes this per-platform-invariant */",
                        d.ctype
                    );
                    format!("SHIM_FIX_{cname}")
                }
                None => cname.to_string(),
            };
            if d.with_droppable_column {
                lines.push(format!("  X({sym}, {cell}, {}) \\", droppable as u8));
            } else {
                lines.push(format!("  X({sym}, {cell}) \\"));
            }
            assert_line(&mut rs, "x86_64", name, x);
            assert_line(&mut rs, "aarch64", name, a);
        }
        let _ = writeln!(h, "#define {}(X) \\", d.macro_name);
        for l in &lines {
            let _ = writeln!(h, "{l}");
        }
        let _ = writeln!(h, "  /* end {} */", d.macro_name);
    }
    let _ = writeln!(h, "\n#endif /* RUST_APE_SHIM_TABLES_H_ */");

    let _ = writeln!(rs, "\nfn main() {{");
    let _ = writeln!(rs, "    println!(\"shim tables check ok (it already passed: the checks are at compile time)\");");
    let _ = writeln!(rs, "}}");

    if args.dry_run {
        println!("{h}");
        return Ok(());
    }
    let h_path = root.join("shim/tables.h");
    let rs_path = root.join("examples/src/bin/shim_tables_check.rs");
    fs::write(&h_path, &h)?;
    fs::write(&rs_path, &rs)?;
    println!("==> wrote {} ({} lines)", h_path.display(), h.lines().count());
    println!("==> wrote {} ({} lines)", rs_path.display(), rs.lines().count());
    Ok(())
}
