//! Compare what the Rust side believes about libc with what cosmo does:
//! Every integer constant of the libc crate against cosmo's value for it,
//! and every libc function the built crates import without going through
//! the shim.

mod allowlist;
mod consumers;
mod cosmo;
mod libc_crate;
mod report;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use self::allowlist::{consumers_of, hot_families, load_audit_file};
use self::consumers::{References, check_std_imports, fn_signatures, libc_imports, suspect_params};
use self::cosmo::{cosmo_values, parse_consts_sh};
use self::libc_crate::{rust_values, scrape_const_names};
use self::report::write_reports;
use crate::util;

/// What the command was asked to do; cli/audit.rs has the flags.
pub struct Options {
    /// cosmopolitan source tree, for libc/sysv/consts.sh
    /// (default: vendor/cosmocc, then the directory it links into)
    pub cosmo_src: Option<PathBuf>,
    /// Project whose built crates are scanned for libc imports
    pub project: PathBuf,
    /// Cargo profile directory of that build
    pub profile: String,
    /// Sweep everything, not only what this build uses: constants nothing
    /// references, and every function the libc crate declares that goes
    /// straight to cosmo
    pub all: bool,
    /// Rewrite patches/std-imports.txt from this build instead of checking
    /// against it
    pub update_std_imports: bool,
}

pub(super) const ARCHES: &[&str] = &["x86_64", "aarch64"];

/// Column of consts.sh for each host an arch's build can land on.
fn hosts(arch: &str) -> &'static [(&'static str, usize)] {
    match arch {
        "x86_64" => &[("linux", 0), ("xnu", 2), ("nt", 7)],
        _ => &[("linux", 1), ("xnu", 3)],
    }
}

/// Functions whose arguments carry no constants at all, so importing them
/// straight from cosmo is fine without anyone looking.
const PLAIN: &[&str] = &[
    "memcpy",
    "memmove",
    "memset",
    "memcmp",
    "bcmp",
    "memchr",
    "memrchr",
    "strlen",
    "strnlen",
    "strcmp",
    "strncmp",
    "strchr",
    "strrchr",
    "strstr",
    "strcpy",
    "strncpy",
    "strdup",
    "malloc",
    "calloc",
    "realloc",
    "free",
    "posix_memalign",
    "aligned_alloc",
    "memalign",
    "malloc_usable_size",
    "abort",
    "exit",
    "_exit",
    "atexit",
    "__cxa_atexit",
    "__cxa_thread_atexit_impl",
    "getenv",
    "setenv",
    "unsetenv",
    "environ",
    "freeaddrinfo",
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Cosmo {
    /// compile-time value, from the headers
    Fixed(i128),
    /// extern const, resolved at startup from the host's column
    Runtime,
    Absent,
    /// declared, but not as an integer constant
    Other,
}

pub(super) struct Syscon {
    pub(super) group: String,
    pub(super) cols: Vec<Option<i128>>,
}

pub fn run(args: &Options) -> Result<()> {
    let root = util::repo_root();
    let work = root.join("target/audit");
    fs::create_dir_all(&work)?;

    // the toolchain ships its consts.sh; an older one only sits in a
    // cosmopolitan checkout, such as the tree it was packaged from
    let cosmocc = fs::canonicalize(root.join("vendor/cosmocc"))?;
    let candidates: Vec<PathBuf> = match &args.cosmo_src {
        Some(p) => vec![p.clone()],
        None => [Some(cosmocc.as_path()), cosmocc.parent()]
            .into_iter()
            .flatten()
            .map(Path::to_path_buf)
            .collect(),
    };
    let Some(consts_sh) = candidates
        .iter()
        .map(|d| d.join("libc/sysv/consts.sh"))
        .find(|p| p.is_file())
    else {
        bail!(
            "no libc/sysv/consts.sh in {}; pass --cosmo-src <cosmopolitan checkout>",
            candidates
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(" or ")
        );
    };
    let syscons = parse_consts_sh(&consts_sh)?;

    let names = scrape_const_names(&root)?;
    println!(
        "==> {} constant names in the libc crate's linux-musl files",
        names.len()
    );

    let signatures = fn_signatures(&root)?;
    let mut fn_names: Vec<String> = signatures.keys().cloned().collect();
    fn_names.sort();
    // functions rustc accepts for the real targets, which settles the cfg
    // questions the source scan can't
    let mut declared: HashSet<String> = HashSet::new();
    let mut musl: HashMap<&str, HashMap<String, i128>> = HashMap::new();
    let mut cosmo: HashMap<&str, HashMap<String, (String, Cosmo)>> = HashMap::new();
    for &arch in ARCHES {
        println!("==> resolving the libc crate's values for {arch}");
        let (m, exist) = rust_values(&root, &work, arch, &names, &fn_names)?;
        declared.extend(exist);
        println!("==> asking cosmocc about {} of them for {arch}", m.len());
        let wanted: Vec<&str> = m.keys().map(String::as_str).collect();
        cosmo.insert(arch, cosmo_values(&root, arch, &wanted)?);
        musl.insert(arch, m);
    }

    let (allow, families) = load_audit_file(&root.join("patches/audit.toml"))?;

    let (imports, cosmo_defined) = libc_imports(&root, &args.project, &args.profile)?;
    let std_imports_ok = check_std_imports(&root, &imports, args.update_std_imports)?;
    let built: HashSet<&str> = imports.values().flatten().map(String::as_str).collect();
    let refs = References::collect(&root, &args.project, &built)?;

    // ---- constants
    let mut rows: Vec<ConstRow> = Vec::new();
    let mut all_names: BTreeSet<&String> = BTreeSet::new();
    for m in musl.values() {
        all_names.extend(m.keys());
    }
    for name in all_names {
        let mut diffs: Vec<String> = Vec::new();
        let mut unsupported: Vec<String> = Vec::new();
        let mut kind = "";
        let mut unknown = false;
        let mut cosmo_cell = String::new();
        for &arch in ARCHES {
            let Some(&lin) = musl[arch].get(name) else {
                continue;
            };
            let Some((spelled, c)) = cosmo[arch].get(name) else {
                continue;
            };
            match *c {
                Cosmo::Fixed(v) => {
                    kind = "fixed";
                    if !same(lin, v) {
                        diffs.push(format!("{arch}: musl {lin} cosmo {v}"));
                    }
                    let _ = write!(cosmo_cell, "{arch}={v} ");
                }
                Cosmo::Runtime => {
                    kind = "runtime";
                    match syscons.get(spelled.as_str()) {
                        Some(sc) => {
                            for &(host, col) in hosts(arch) {
                                match sc.cols.get(col).copied().flatten() {
                                    Some(v) if same(lin, v) => {}
                                    // 0 and -1 are how cosmo says a host lacks the thing.
                                    // Passing the Linux value through is only wrong when
                                    // it means something else there.
                                    Some(v @ (0 | -1)) => match collides(&syscons, sc, spelled, col, lin) {
                                        Some(other) => diffs.push(format!(
                                            "{arch}/{host}: musl {lin} cosmo {v}, and {lin} is {other} there"
                                        )),
                                        None => unsupported.push(format!("{arch}/{host}")),
                                    },
                                    Some(v) => diffs.push(format!("{arch}/{host}: musl {lin} cosmo {v}")),
                                    None => unknown = true,
                                }
                            }
                            let _ = write!(cosmo_cell, "{arch}:{} ", sc.group);
                        }
                        None => unknown = true,
                    }
                }
                Cosmo::Absent => kind = if kind.is_empty() { "absent" } else { kind },
                Cosmo::Other => kind = if kind.is_empty() { "other" } else { kind },
            }
        }
        let verdict = if kind == "absent" || kind == "other" {
            kind
        } else if diffs.is_empty() && !unknown {
            if unsupported.is_empty() {
                "same"
            } else {
                "unsupported"
            }
        } else if consumers_of(&families, name).is_some_and(|fns| {
            !fns.iter().any(|f| {
                imports.contains_key(f.as_str()) || imports.contains_key(&format!("__ape_shim_{f}"))
            })
        }) {
            // nothing in this build calls a function that takes it
            "idle"
        } else if allow.consts.matches(name) {
            "allowed"
        } else if diffs.is_empty() {
            "unknown"
        } else {
            "GAP"
        };
        rows.push(ConstRow {
            name: name.clone(),
            kind,
            verdict,
            diffs,
            cosmo_cell,
            users: refs.users(name),
        });
    }

    // ---- functions
    let hot = hot_families(&families, &rows);
    // what is wrong with sending `func` straight to cosmo, if anything:
    // (is a family with a differing value involved, what to show)
    let judge = |func: &String| -> Option<(bool, String)> {
        if func.starts_with("__ape_shim_") || PLAIN.contains(&func.as_str()) {
            return None;
        }
        let differing: Vec<&str> = hot
            .iter()
            .filter(|(_, consumers)| consumers.contains(func))
            .map(|(label, _)| label.as_str())
            .collect();
        if !differing.is_empty() {
            return Some((true, differing.join(" ")));
        }
        // no family lists it: look at the signature for anything that
        // could carry a constant and have a person decide
        if families.iter().any(|f| f.consumers.contains(func)) {
            return None;
        }
        let suspects = suspect_params(&signatures.get(func.as_str())?.params);
        (!suspects.is_empty()).then(|| (false, suspects.join(", ")))
    };

    let mut fn_rows: Vec<FnRow> = Vec::new();
    for (func, crates) in &imports {
        let Some((in_family, note)) = judge(func) else {
            continue;
        };
        let verdict = match (allow.fns.matches(func), in_family) {
            (true, _) => "allowed",
            (false, true) => "GAP",
            (false, false) => "review",
        };
        fn_rows.push(FnRow {
            name: func.clone(),
            verdict,
            families: note,
            crates: crates.iter().cloned().collect::<Vec<_>>().join(" "),
        });
    }

    // --all: the same questions for what the libc crate declares and this
    // build happens not to call
    if args.all {
        let mut unused: Vec<&String> = signatures
            .iter()
            .filter(|(name, decl)| {
                !decl.redirected
                    && declared.contains(*name)
                    && cosmo_defined.contains(*name)
                    && !imports.contains_key(*name)
            })
            .map(|(name, _)| name)
            .collect();
        unused.sort();
        for func in unused {
            let Some((in_family, note)) = judge(func) else {
                continue;
            };
            let verdict = match (allow.fns.matches(func), in_family) {
                (true, _) => "allowed",
                (false, true) => "unused GAP",
                (false, false) => "unused review",
            };
            fn_rows.push(FnRow {
                name: func.clone(),
                verdict,
                families: note,
                crates: String::new(),
            });
        }
    }

    write_reports(&work, &rows, &fn_rows, &imports)?;

    // ---- summary
    let count = |v: &str| rows.iter().filter(|r| r.verdict == v).count();
    println!();
    println!(
        "constants: {} same, {} allowed or without a caller, {} missing on some host only, {} absent from cosmo, {} not integers there",
        count("same"),
        count("allowed") + count("idle"),
        count("unsupported"),
        count("absent"),
        count("other"),
    );
    let mut failed = false;
    for (title, verdict) in [
        (
            "value differs on some host and nothing translates it",
            "GAP",
        ),
        ("runtime constant with no row in consts.sh", "unknown"),
    ] {
        let hits: Vec<&ConstRow> = rows
            .iter()
            .filter(|r| r.verdict == verdict && (args.all || !r.users.is_empty()))
            .collect();
        let silent = rows.iter().filter(|r| r.verdict == verdict).count() - hits.len();
        if hits.is_empty() {
            if silent > 0 {
                println!("\n{silent} constants nothing references: {title} (--all lists them)");
            }
            continue;
        }
        println!("\n{title}: {}", hits.len());
        for r in &hits {
            // what --all adds is a survey, not a verdict on this build
            failed |= !r.users.is_empty();
            println!(
                "  {:<28} used by {}",
                r.name,
                if r.users.is_empty() { "-" } else { &r.users }
            );
            for d in &r.diffs {
                println!("      {d}");
            }
        }
        if silent > 0 {
            println!("  (+{silent} more that nothing references; --all lists them)");
        }
    }
    let gaps: Vec<&FnRow> = fn_rows.iter().filter(|r| r.verdict == "GAP").collect();
    if !gaps.is_empty() {
        failed = true;
        println!(
            "\nfunctions imported straight from cosmo that take a differing family: {}",
            gaps.len()
        );
        for r in gaps {
            println!("  {:<28} [{}] from {}", r.name, r.families, r.crates);
        }
    }
    let review: Vec<&FnRow> = fn_rows.iter().filter(|r| r.verdict == "review").collect();
    if !review.is_empty() {
        failed = true;
        println!(
            "\nfunctions imported straight from cosmo that no family lists, with arguments that could carry a constant: {}",
            review.len()
        );
        for r in review {
            println!("  {:<28} ({}) from {}", r.name, r.families, r.crates);
        }
        println!(
            "  add each to a [[family]] in patches/audit.toml, or to its [allow.fn] with a reason"
        );
    }
    for (title, verdict) in [
        (
            "declared by the libc crate, not called in this build, and taking a differing family",
            "unused GAP",
        ),
        (
            "declared by the libc crate, not called in this build, no family lists them",
            "unused review",
        ),
    ] {
        let hits: Vec<&FnRow> = fn_rows.iter().filter(|r| r.verdict == verdict).collect();
        if hits.is_empty() {
            continue;
        }
        println!("\n{title}: {}", hits.len());
        for r in hits {
            println!("  {:<32} {}", r.name, r.families);
        }
    }

    let stale_fns = if args.all {
        allow.fns.unused()
    } else {
        Vec::new()
    };
    let stale: Vec<String> = allow
        .consts
        .unused()
        .into_iter()
        .map(|p| format!("const {p}"))
        .chain(stale_fns.into_iter().map(|p| format!("fn {p}")))
        .collect();
    if !stale.is_empty() {
        println!(
            "\nwarning: patches/audit.toml allow entries that silenced nothing in this build:"
        );
        for e in &stale {
            println!("  {e}");
        }
    }
    println!("\nfull tables: {}", work.join("consts.tsv").display());
    println!("             {}", work.join("functions.tsv").display());
    if failed {
        bail!("audit found gaps; fix them or explain them in patches/audit.toml");
    }
    if !std_imports_ok {
        bail!(
            "std's libc imports changed; review them, then `cargo xtask audit --update-std-imports`"
        );
    }
    println!("audit ok");
    Ok(())
}

pub(super) struct ConstRow {
    pub(super) name: String,
    pub(super) kind: &'static str,
    pub(super) verdict: &'static str,
    pub(super) diffs: Vec<String>,
    pub(super) cosmo_cell: String,
    pub(super) users: String,
}

pub(super) struct FnRow {
    pub(super) name: String,
    pub(super) verdict: &'static str,
    pub(super) families: String,
    pub(super) crates: String,
}

/// Equal, or equal as bit patterns: musl types ioctl requests as c_int where
/// cosmo has unsigned, and SIG_ERR is usize::MAX on one side, -1 on the other.
pub(super) fn same(a: i128, b: i128) -> bool {
    let fits = |v: i128| (-(1i128 << 31)..(1i128 << 32)).contains(&v);
    a == b || a as u64 == b as u64 || (fits(a) && fits(b) && a as u32 == b as u32)
}

/// Another constant of the same consts.sh group whose value on this host is
/// what Linux uses for `name`.
fn collides<'a>(
    syscons: &'a HashMap<String, Syscon>,
    sc: &Syscon,
    name: &str,
    col: usize,
    lin: i128,
) -> Option<&'a str> {
    syscons
        .iter()
        .filter(|(n, other)| other.group == sc.group && n.as_str() != name)
        .filter(|(_, other)| other.cols.get(col).copied().flatten() == Some(lin))
        .map(|(n, _)| n.as_str())
        .min()
}

// ---------------------------------------------------------------- libc crate
