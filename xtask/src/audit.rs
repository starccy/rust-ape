//! Compare what the Rust side believes about libc with what cosmo does:
//! Every integer constant of the libc crate against cosmo's value for it,
//! and every libc function the built crates import without going through
//! the shim.

use crate::genshim;
use crate::util;
use anyhow::{Context, Result, bail};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Args)]
pub struct AuditArgs {
    /// cosmopolitan source tree, for libc/sysv/consts.sh
    /// (default: the directory vendor/cosmocc links into)
    #[arg(long, env = "RUST_APE_COSMO_SRC")]
    pub cosmo_src: Option<PathBuf>,

    /// Project whose built crates are scanned for libc imports
    #[arg(long, default_value = "examples")]
    pub project: PathBuf,

    /// Cargo profile directory of that build
    #[arg(long, default_value = "debug")]
    pub profile: String,

    /// Sweep everything, not only what this build uses: constants nothing
    /// references, and every function the libc crate declares that goes
    /// straight to cosmo
    #[arg(long)]
    pub all: bool,
}

const ARCHES: &[&str] = &["x86_64", "aarch64"];

/// Column of consts.sh for each host an arch's build can land on.
fn hosts(arch: &str) -> &'static [(&'static str, usize)] {
    match arch {
        "x86_64" => &[("linux", 0), ("xnu", 2), ("nt", 7)],
        _ => &[("linux", 1), ("xnu", 3)],
    }
}

/// One [[family]] of patches/audit.toml.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Family {
    prefixes: Vec<String>,
    /// prefixes that take names back out
    #[serde(default)]
    except: Vec<String>,
    #[serde(rename = "functions")]
    consumers: Vec<String>,
}

/// patches/audit.toml
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditFile {
    allow: AllowFile,
    family: Vec<Family>,
}

/// name = "why"; a trailing `*` on the name matches a prefix
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowFile {
    #[serde(rename = "const", default)]
    consts: BTreeMap<String, String>,
    #[serde(rename = "fn", default)]
    fns: BTreeMap<String, String>,
}

fn load_audit_file(path: &Path) -> Result<(Allow, Vec<Family>)> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file: AuditFile = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let patterns = |kind: &str, m: BTreeMap<String, String>| -> Result<Patterns> {
        if let Some((name, _)) = m.iter().find(|(_, why)| why.trim().is_empty()) {
            bail!("{}: allow.{kind}.{name} needs a reason", path.display());
        }
        Ok(Patterns(m.into_keys().map(|k| (k, Default::default())).collect()))
    };
    for f in &file.family {
        if f.prefixes.is_empty() || f.consumers.is_empty() {
            bail!("{}: a family needs a prefix and a function", path.display());
        }
    }
    let allow = Allow {
        consts: patterns("const", file.allow.consts)?,
        fns: patterns("fn", file.allow.fns)?,
    };
    Ok((allow, file.family))
}

/// Functions whose arguments carry no constants at all, so importing them
/// straight from cosmo is fine without anyone looking.
const PLAIN: &[&str] = &[
    "memcpy", "memmove", "memset", "memcmp", "bcmp", "memchr", "memrchr", "strlen", "strnlen",
    "strcmp", "strncmp", "strchr", "strrchr", "strstr", "strcpy", "strncpy", "strdup",
    "malloc", "calloc", "realloc", "free", "posix_memalign", "aligned_alloc", "memalign",
    "malloc_usable_size", "abort", "exit", "_exit", "atexit", "__cxa_atexit",
    "__cxa_thread_atexit_impl", "getenv", "setenv", "unsetenv", "environ", "freeaddrinfo",
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cosmo {
    /// compile-time value, from the headers
    Fixed(i128),
    /// extern const, resolved at startup from the host's column
    Runtime,
    Absent,
    /// declared, but not as an integer constant
    Other,
}

struct Syscon {
    group: String,
    cols: Vec<Option<i128>>,
}

pub fn run(args: &AuditArgs) -> Result<()> {
    let root = util::repo_root();
    let work = root.join("target/audit");
    fs::create_dir_all(&work)?;

    let cosmo_src = match &args.cosmo_src {
        Some(p) => p.clone(),
        None => fs::canonicalize(root.join("vendor/cosmocc"))?
            .parent()
            .map(Path::to_path_buf)
            .context("vendor/cosmocc has no parent directory")?,
    };
    let consts_sh = cosmo_src.join("libc/sysv/consts.sh");
    if !consts_sh.is_file() {
        bail!(
            "{} not found; pass --cosmo-src <cosmopolitan checkout>",
            consts_sh.display()
        );
    }
    let syscons = parse_consts_sh(&consts_sh)?;

    let names = scrape_const_names(&root)?;
    println!("==> {} constant names in the libc crate's linux-musl files", names.len());

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

    let covered = shim_covered(&root)?;
    let (allow, families) = load_audit_file(&root.join("patches/audit.toml"))?;

    let (imports, cosmo_defined) = libc_imports(&root, &args.project, &args.profile)?;
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
            let Some(&lin) = musl[arch].get(name) else { continue };
            let Some((spelled, c)) = cosmo[arch].get(name) else { continue };
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
            if unsupported.is_empty() { "same" } else { "unsupported" }
        } else if covered.contains(name.as_str())
            // PF_X is AF_X under its older name
            || name.strip_prefix("PF_").is_some_and(|rest| {
                let af = format!("AF_{rest}");
                covered.contains(&af) && ARCHES.iter().all(|a| musl[a].get(&af) == musl[a].get(name))
            })
        {
            "covered"
        } else if consumers_of(&families, name).is_some_and(|fns| {
            !fns.iter().any(|f| imports.contains_key(f.as_str()) || imports.contains_key(&format!("__ape_shim_{f}")))
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
        let Some((in_family, note)) = judge(func) else { continue };
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
            let Some((in_family, note)) = judge(func) else { continue };
            let verdict = match (allow.fns.matches(func), in_family) {
                (true, _) => "allowed",
                (false, true) => "unused GAP",
                (false, false) => "unused review",
            };
            fn_rows.push(FnRow { name: func.clone(), verdict, families: note, crates: String::new() });
        }
    }

    write_reports(&work, &rows, &fn_rows, &imports)?;

    // ---- summary
    let count = |v: &str| rows.iter().filter(|r| r.verdict == v).count();
    println!();
    println!(
        "constants: {} same, {} covered by shim/tables.h, {} allowed or without a caller, {} missing on some host only, {} absent from cosmo, {} not integers there",
        count("same"), count("covered"), count("allowed") + count("idle"), count("unsupported"), count("absent"), count("other"),
    );
    let mut failed = false;
    for (title, verdict) in [
        ("value differs on some host and nothing translates it", "GAP"),
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
            println!("  {:<28} used by {}", r.name, if r.users.is_empty() { "-" } else { &r.users });
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
        println!("\nfunctions imported straight from cosmo that take a differing family: {}", gaps.len());
        for r in gaps {
            println!("  {:<28} [{}] from {}", r.name, r.families, r.crates);
        }
    }
    let review: Vec<&FnRow> = fn_rows.iter().filter(|r| r.verdict == "review").collect();
    if !review.is_empty() {
        failed = true;
        println!("\nfunctions imported straight from cosmo that no family lists, with arguments that could carry a constant: {}", review.len());
        for r in review {
            println!("  {:<28} ({}) from {}", r.name, r.families, r.crates);
        }
        println!("  add each to a [[family]] in patches/audit.toml, or to its [allow.fn] with a reason");
    }
    for (title, verdict) in [
        ("declared by the libc crate, not called in this build, and taking a differing family", "unused GAP"),
        ("declared by the libc crate, not called in this build, no family lists them", "unused review"),
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
    let stale: Vec<String> = allow
        .consts
        .unused()
        .into_iter()
        .map(|p| format!("const {p}"))
        .chain(allow.fns.unused().into_iter().map(|p| format!("fn {p}")))
        .collect();
    if !stale.is_empty() {
        println!("\nwarning: patches/audit.toml allow entries that silenced nothing in this build:");
        for e in &stale {
            println!("  {e}");
        }
    }
    println!("\nfull tables: {}", work.join("consts.tsv").display());
    println!("             {}", work.join("functions.tsv").display());
    if failed {
        bail!("audit found gaps; fix them or explain them in patches/audit.toml");
    }
    println!("audit ok");
    Ok(())
}

struct ConstRow {
    name: String,
    kind: &'static str,
    verdict: &'static str,
    diffs: Vec<String>,
    cosmo_cell: String,
    users: String,
}

struct FnRow {
    name: String,
    verdict: &'static str,
    families: String,
    crates: String,
}

/// Equal, or equal as bit patterns: musl types ioctl requests as c_int where
/// cosmo has unsigned, and SIG_ERR is usize::MAX on one side, -1 on the other.
fn same(a: i128, b: i128) -> bool {
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

fn scrape_const_names(root: &Path) -> Result<Vec<String>> {
    let src = root.join("vendor/patches/libc/src");
    let mut files: Vec<PathBuf> = ARCHES
        .iter()
        .flat_map(|a| genshim::libc_search_paths(root, a))
        .collect();
    for sub in ["new/linux_uapi", "new/musl", "new/common"] {
        walk(&src.join(sub), "rs", &mut files)?;
    }
    let mut names = BTreeSet::new();
    for f in files {
        let Ok(text) = fs::read_to_string(&f) else { continue };
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("pub const ") else { continue };
            let Some((name, _)) = rest.split_once(':') else { continue };
            let name = name.trim();
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                names.insert(name.to_string());
            }
        }
    }
    Ok(names.into_iter().collect())
}

fn walk(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(rd) = fs::read_dir(dir) else { return Ok(()) };
    for e in rd {
        let p = e?.path();
        if p.is_dir() {
            walk(&p, ext, out)?;
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
    Ok(())
}

/// Build a crate of `static AUDIT_<name>: i128 = libc::<name> as i128` for the
/// real target spec and read the values back out of the LLVM IR. Names that
/// don't exist for this target, or aren't integers, fall out through rustc's
/// error list.
fn rust_values(
    root: &Path,
    work: &Path,
    arch: &str,
    names: &[String],
    fns: &[String],
) -> Result<(HashMap<String, i128>, HashSet<String>)> {
    let dir = work.join("rs");
    fs::create_dir_all(dir.join("src"))?;
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"audit-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
             [dependencies]\nlibc = {{ path = \"{}\", default-features = false }}\n\n[workspace]\n",
            root.join("vendor/patches/libc").display()
        ),
    )?;
    const HEADER: &str = "#![no_std]\n#![allow(deprecated, overflowing_literals, unused_imports)]\n";
    let header_lines = HEADER.lines().count();
    let target = root.join(format!("generated/{arch}-unknown-linux-musl.json"));
    // one probe per line, constants first, then functions: naming a function
    // only compiles when the crate declares it for this target
    let mut live: Vec<(bool, &String)> =
        names.iter().map(|n| (false, n)).chain(fns.iter().map(|n| (true, n))).collect();
    for _round in 0..8 {
        let mut text = String::from(HEADER);
        for (is_fn, n) in &live {
            if *is_fn {
                let _ = writeln!(text, "pub fn audit_fn_{n}() {{ let _ = libc::{n}; }}");
            } else {
                let _ = writeln!(text, "#[unsafe(no_mangle)] pub static AUDIT_{n}: i128 = libc::{n} as i128;");
            }
        }
        fs::write(dir.join("src/lib.rs"), text)?;
        let deps = dir.join(format!("target/{arch}-unknown-linux-musl/debug/deps"));
        if let Ok(rd) = fs::read_dir(&deps) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "ll") {
                    let _ = fs::remove_file(p);
                }
            }
        }
        let out = Command::new("cargo")
            .current_dir(&dir)
            .env("CARGO_INCREMENTAL", "0")
            .args(["rustc", "--lib", "--quiet", "--message-format=short", "--target"])
            .arg(&target)
            .args(["-Zbuild-std=core", "-Zjson-target-spec", "--", "--emit=llvm-ir"])
            .output()
            .context("running cargo for the constant probe")?;
        if out.status.success() {
            let ll = fs::read_dir(&deps)?
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.extension().is_some_and(|x| x == "ll")
                        && p.file_name().is_some_and(|f| f.to_string_lossy().starts_with("audit_probe"))
                })
                .context("the probe built but left no .ll file")?;
            let exist = live.iter().filter(|(is_fn, _)| *is_fn).map(|(_, n)| (*n).clone()).collect();
            return Ok((parse_llvm_ir(&fs::read_to_string(ll)?)?, exist));
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut bad: HashSet<usize> = HashSet::new();
        for line in stderr.lines() {
            let Some(rest) = line.strip_prefix("src/lib.rs:") else { continue };
            if !rest.contains("error") {
                continue;
            }
            if let Some(n) = rest.split(':').next().and_then(|n| n.parse::<usize>().ok()) {
                bad.insert(n);
            }
        }
        if bad.is_empty() {
            bail!("the constant probe failed in a way that names no line:\n{stderr}");
        }
        live = live
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !bad.contains(&(i + 1 + header_lines)))
            .map(|(_, n)| n)
            .collect();
    }
    bail!("the constant probe for {arch} still doesn't build after 8 rounds of pruning")
}

/// `@AUDIT_X = ... constant [16 x i8] c"\04\00..."` or `zeroinitializer`.
fn parse_llvm_ir(ir: &str) -> Result<HashMap<String, i128>> {
    let mut out = HashMap::new();
    for line in ir.lines() {
        let Some(rest) = line.strip_prefix("@AUDIT_") else { continue };
        let Some((name, rest)) = rest.split_once(' ') else { continue };
        let value = if rest.contains("zeroinitializer") {
            0
        } else if let Some((_, s)) = rest.split_once("c\"") {
            let s = s.split('"').next().unwrap_or("");
            let mut bytes = Vec::with_capacity(16);
            let raw = s.as_bytes();
            let mut i = 0;
            while i < raw.len() {
                if raw[i] == b'\\' && raw.get(i + 1) == Some(&b'\\') {
                    bytes.push(b'\\');
                    i += 2;
                } else if raw[i] == b'\\' && i + 2 < raw.len() {
                    let hex = std::str::from_utf8(&raw[i + 1..i + 3])?;
                    bytes.push(u8::from_str_radix(hex, 16)?);
                    i += 3;
                } else {
                    bytes.push(raw[i]);
                    i += 1;
                }
            }
            if bytes.len() != 16 {
                bail!("unexpected initializer width for AUDIT_{name}: {} bytes", bytes.len());
            }
            i128::from_le_bytes(bytes.try_into().unwrap())
        } else if let Some((_, v)) = rest.split_once("constant i128 ") {
            v.split(|c: char| c == ',' || c == ' ').next().unwrap_or("").parse()?
        } else {
            bail!("can't read the initializer of AUDIT_{name}: {line}");
        };
        out.insert(name.to_string(), value);
    }
    Ok(out)
}

// --------------------------------------------------------------------- cosmo

const C_HEADERS: &[&str] = &[
    "errno.h", "fcntl.h", "unistd.h", "signal.h", "spawn.h", "sched.h", "pthread.h", "poll.h",
    "termios.h", "time.h", "dirent.h", "dlfcn.h", "limits.h", "locale.h", "langinfo.h", "netdb.h",
    "syslog.h", "semaphore.h", "fnmatch.h", "glob.h", "regex.h", "grp.h", "pwd.h", "stdio.h",
    "stdlib.h", "ifaddrs.h", "elf.h", "utime.h", "wordexp.h", "nl_types.h", "iconv.h",
    "sys/file.h", "sys/stat.h", "sys/socket.h", "sys/un.h", "sys/mman.h", "sys/wait.h",
    "sys/resource.h", "sys/ioctl.h", "sys/uio.h", "sys/time.h", "sys/times.h", "sys/utsname.h",
    "sys/statvfs.h", "sys/statfs.h", "sys/select.h", "sys/sysinfo.h", "sys/xattr.h", "sys/random.h",
    "sys/auxv.h", "sys/ptrace.h", "sys/prctl.h", "sys/ipc.h", "sys/shm.h", "sys/sem.h", "sys/msg.h",
    "sys/mount.h", "sys/timerfd.h", "sys/eventfd.h", "sys/inotify.h", "sys/epoll.h", "sys/reboot.h",
    "sys/syscall.h", "sys/vfs.h", "sys/param.h", "sys/sysmacros.h", "sys/personality.h",
    "netinet/in.h", "netinet/tcp.h", "netinet/udp.h", "netinet/ip.h", "netinet/ip_icmp.h",
    "arpa/inet.h", "net/if.h", "net/if_arp.h", "net/ethernet.h",
];

/// Compile `const __int128 AUDIT_<name> = (__int128)(<name>)` for every name
/// to assembly and read the values back. What gcc refuses tells the rest:
/// undeclared, a runtime constant, or not an integer.
fn cosmo_values(root: &Path, arch: &str, names: &[&str]) -> Result<HashMap<String, (String, Cosmo)>> {
    let mut out: HashMap<String, (String, Cosmo)> = HashMap::new();
    let mut spelled: Vec<(String, String)> = names
        .iter()
        .map(|n| (n.to_string(), genshim::cosmo_name(n).to_string()))
        .collect();
    // second pass: what was undeclared gets one more try as _NAME
    for pass in 0..2 {
        let got = cosmo_probe(root, arch, &spelled)?;
        let mut retry = Vec::new();
        for (name, cname) in &spelled {
            let c = got[cname.as_str()];
            if c == Cosmo::Absent && pass == 0 {
                retry.push((name.clone(), format!("_{cname}")));
            }
            if pass == 0 || c != Cosmo::Absent {
                out.insert(name.clone(), (cname.clone(), c));
            }
        }
        spelled = retry;
        if spelled.is_empty() {
            break;
        }
    }
    Ok(out)
}

fn cosmo_probe(root: &Path, arch: &str, spelled: &[(String, String)]) -> Result<HashMap<String, Cosmo>> {
    let cc = root.join(format!("vendor/cosmocc/bin/{arch}-unknown-cosmo-cc"));
    let inc = root.join("vendor/cosmocc/include");
    let mut header = String::from("#define _COSMO_SOURCE\n#define _GNU_SOURCE\n");
    for h in C_HEADERS {
        let _ = writeln!(header, "#if __has_include(<{h}>)\n#include <{h}>\n#endif");
    }
    let mut consts_headers = Vec::new();
    walk(&inc.join("libc/sysv/consts"), "h", &mut consts_headers)?;
    consts_headers.sort();
    for h in consts_headers {
        let rel = h.strip_prefix(&inc).unwrap().display().to_string();
        // assembler-only
        if rel.ends_with("syscon.internal.h") {
            continue;
        }
        let _ = writeln!(header, "#include <{rel}>");
    }
    let header_lines = header.lines().count();

    // /dev/shm: plain tmp directories can hand the compiler ciphertext here
    let src = PathBuf::from(format!("/dev/shm/rust-ape-audit-{arch}.c"));
    let asm = PathBuf::from(format!("/dev/shm/rust-ape-audit-{arch}.s"));
    let mut result: HashMap<String, Cosmo> = HashMap::new();
    let mut live: Vec<&str> = spelled.iter().map(|(_, c)| c.as_str()).collect();
    live.sort();
    live.dedup();
    for _round in 0..8 {
        let mut text = header.clone();
        for c in &live {
            let _ = writeln!(text, "const __int128 AUDIT_{c} = (__int128)({c});");
        }
        fs::write(&src, text)?;
        let out = Command::new(&cc)
            .args(["-S", "-O0", "-w", "-fmax-errors=0", "-o"])
            .arg(&asm)
            .arg(&src)
            .output()
            .context("running cosmocc for the constant probe")?;
        if out.status.success() {
            for (name, v) in parse_asm(&fs::read_to_string(&asm)?)? {
                result.insert(name, Cosmo::Fixed(v));
            }
            let _ = fs::remove_file(&src);
            let _ = fs::remove_file(&asm);
            return Ok(result);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let prefix = format!("{}:", src.display());
        let mut bad: HashMap<usize, Cosmo> = HashMap::new();
        for line in stderr.lines() {
            let Some(rest) = line.strip_prefix(&prefix) else { continue };
            let Some((lineno, rest)) = rest.split_once(':') else { continue };
            let Ok(lineno) = lineno.parse::<usize>() else { continue };
            if !rest.contains(" error: ") {
                continue;
            }
            let why = if rest.contains("undeclared") {
                Cosmo::Absent
            } else if rest.contains("substitute constant") || rest.contains("is not constant") {
                Cosmo::Runtime
            } else {
                Cosmo::Other
            };
            // an undeclared name also trips follow-up errors; keep the first
            bad.entry(lineno).or_insert(why);
        }
        if bad.is_empty() {
            bail!("cosmocc failed without naming a probe line:\n{stderr}");
        }
        let mut next = Vec::new();
        for (i, c) in live.iter().enumerate() {
            match bad.get(&(i + 1 + header_lines)) {
                Some(why) => {
                    result.insert(c.to_string(), *why);
                }
                None => next.push(*c),
            }
        }
        live = next;
    }
    bail!("the cosmocc probe for {arch} still doesn't compile after 8 rounds of pruning")
}

/// `AUDIT_X:` followed by two 64-bit words (low, high) or `.zero 16`.
fn parse_asm(asm: &str) -> Result<Vec<(String, i128)>> {
    let mut out = Vec::new();
    let mut lines = asm.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(name) = line.strip_prefix("AUDIT_").and_then(|l| l.strip_suffix(':')) else {
            continue;
        };
        let mut words: Vec<i128> = Vec::new();
        while words.len() < 2 {
            let Some(l) = lines.next() else { break };
            let mut it = l.split_whitespace();
            match (it.next(), it.next()) {
                (Some(".quad" | ".xword"), Some(v)) => words.push(v.parse::<i128>()?),
                (Some(".zero"), Some(n)) => {
                    for _ in 0..n.parse::<usize>()? / 8 {
                        words.push(0);
                    }
                }
                _ => bail!("unexpected directive after AUDIT_{name}: {l}"),
            }
        }
        if words.len() < 2 {
            bail!("short initializer for AUDIT_{name}");
        }
        let low = words[0] as u64 as u128;
        let high = words[1] as u64 as u128;
        out.push((name.to_string(), ((high << 64) | low) as i128));
    }
    Ok(out)
}

fn parse_consts_sh(path: &Path) -> Result<HashMap<String, Syscon>> {
    let text = fs::read_to_string(path)?;
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let mut it = line.split_whitespace();
        if it.next() != Some("syscon") {
            continue;
        }
        let (Some(group), Some(name)) = (it.next(), it.next()) else { continue };
        let cols: Vec<Option<i128>> = it.take(8).map(parse_c_int).collect();
        out.insert(name.to_string(), Syscon { group: group.to_string(), cols });
    }
    Ok(out)
}

fn parse_c_int(s: &str) -> Option<i128> {
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let s = s.trim_end_matches(['u', 'U', 'l', 'L']);
    let v = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i128::from_str_radix(h, 16).ok()?
    } else if let Some(b) = s.strip_prefix("0b") {
        i128::from_str_radix(b, 2).ok()?
    } else if s.len() > 1 && s.starts_with('0') {
        i128::from_str_radix(&s[1..], 8).ok()?
    } else {
        s.parse().ok()?
    };
    Some(if neg { -v } else { v })
}

// ---------------------------------------------------------------------- shim

/// Every name shim/tables.h carries a Linux value for.
fn shim_covered(root: &Path) -> Result<HashSet<String>> {
    let text = fs::read_to_string(root.join("shim/tables.h"))?;
    let mut out = HashSet::new();
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("#define SHIM_LIN_") {
            if let Some(name) = rest.split_whitespace().next() {
                out.insert(name.to_string());
            }
        }
    }
    Ok(out)
}

struct Patterns(Vec<(String, std::cell::Cell<bool>)>);

impl Patterns {
    /// Also remembers which entries ever matched, for `unused`.
    fn matches(&self, name: &str) -> bool {
        let mut any = false;
        for (p, hit) in &self.0 {
            let m = match p.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => p == name,
            };
            if m {
                hit.set(true);
                any = true;
            }
        }
        any
    }

    fn unused(&self) -> Vec<&str> {
        self.0.iter().filter(|(_, hit)| !hit.get()).map(|(p, _)| p.as_str()).collect()
    }
}

struct Allow {
    consts: Patterns,
    fns: Patterns,
}

// ----------------------------------------------------------------- consumers

/// Who mentions a constant by name: std, or crates in the project's
/// dependency tree. Only used to rank findings.
struct References {
    std: HashSet<String>,
    crates: BTreeMap<String, HashSet<String>>,
}

impl References {
    fn collect(root: &Path, project: &Path, built: &HashSet<&str>) -> Result<References> {
        let mut std = HashSet::new();
        for sub in ["std/src", "core/src", "alloc/src", "panic_unwind/src", "panic_abort/src"] {
            let mut files = Vec::new();
            walk(&root.join("vendor/library").join(sub), "rs", &mut files)?;
            for f in files {
                idents(&f, &mut std);
            }
        }
        let mut crates = BTreeMap::new();
        let manifest = root.join(project).join("Cargo.toml");
        let meta = util::capture(
            Command::new("cargo")
                .args(["metadata", "--format-version=1", "--manifest-path"])
                .arg(&manifest),
        )?;
        let meta: serde_json::Value = serde_json::from_str(&meta)?;
        for pkg in meta["packages"].as_array().into_iter().flatten() {
            let name = pkg["name"].as_str().unwrap_or("");
            // crates that carry their own tables (linux-raw-sys, windows-sys)
            // or never made it into this build say nothing about libc's values
            let uses_libc = pkg["dependencies"]
                .as_array()
                .is_some_and(|d| d.iter().any(|d| d["name"] == "libc" && d["kind"].is_null()));
            // the project itself carries the generated table check, which
            // names every constant the shim knows
            let local = pkg["source"].is_null();
            if name == "libc" || local || !uses_libc || !built.contains(name.replace('-', "_").as_str()) {
                continue;
            }
            let Some(dir) = pkg["manifest_path"].as_str().and_then(|p| Path::new(p).parent()) else {
                continue;
            };
            let mut files = Vec::new();
            walk(&dir.join("src"), "rs", &mut files)?;
            let mut set = HashSet::new();
            for f in files {
                idents(&f, &mut set);
            }
            crates.insert(name.to_string(), set);
        }
        Ok(References { std, crates })
    }

    fn users(&self, name: &str) -> String {
        let mut out = Vec::new();
        if self.std.contains(name) {
            out.push("std");
        }
        for (krate, set) in &self.crates {
            if set.contains(name) {
                out.push(krate.as_str());
            }
        }
        out.join(" ")
    }
}

/// Upper-case identifiers of one source file.
fn idents(path: &Path, out: &mut HashSet<String>) {
    let Ok(text) = fs::read_to_string(path) else { return };
    let mut cur = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_alphanumeric() || c == '_' {
            cur.push(c);
            continue;
        }
        if cur.len() > 2
            && cur.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
            && !cur.chars().any(|c| c.is_ascii_lowercase())
        {
            out.insert(std::mem::take(&mut cur));
        }
        cur.clear();
    }
}

/// Undefined symbols of every rlib the project built that libcosmo.a
/// defines, with the crates importing each.
fn libc_imports(
    root: &Path,
    project: &Path,
    profile: &str,
) -> Result<(BTreeMap<String, BTreeSet<String>>, HashSet<String>)> {
    let deps = root
        .join(project)
        .join(format!("target/x86_64-unknown-linux-musl/{profile}/deps"));
    let mut rlibs = Vec::new();
    walk(&deps, "rlib", &mut rlibs)?;
    if rlibs.is_empty() {
        bail!(
            "no rlibs under {}; run `cargo xtask build {}` first",
            deps.display(),
            project.display()
        );
    }
    // cargo leaves older builds of a crate next to the current one
    let mut newest: HashMap<String, (std::time::SystemTime, PathBuf)> = HashMap::new();
    for p in rlibs {
        let mtime = fs::metadata(&p)?.modified()?;
        let krate = crate_of(&p);
        if newest.get(&krate).is_none_or(|(t, _)| *t < mtime) {
            newest.insert(krate, (mtime, p));
        }
    }
    let rlibs: Vec<PathBuf> = newest.into_values().map(|(_, p)| p).collect();

    let nm = root.join("vendor/cosmocc/bin/x86_64-linux-cosmo-nm");
    let defined: HashSet<String> = nm_lines(
        util::ape_command(&nm)
            .args(["--defined-only", "-g"])
            .arg(root.join("vendor/cosmocc/x86_64-linux-cosmo/lib/libcosmo.a")),
    )?
    .iter()
    .filter_map(|l| l.split_whitespace().last().map(str::to_string))
    .collect();

    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for chunk in rlibs.chunks(200) {
        for line in nm_lines(util::ape_command(&nm).args(["-u", "-A"]).args(chunk))? {
            // path/libfoo-hash.rlib:member.o:                 U symbol
            let Some(sym) = line.split_whitespace().last() else { continue };
            if !defined.contains(sym) && !sym.starts_with("__ape_shim_") {
                continue;
            }
            let file = line.split(".rlib:").next().unwrap_or("");
            out.entry(sym.to_string()).or_default().insert(crate_of(Path::new(file)));
        }
    }
    Ok((out, defined))
}

/// `deps/libfoo_bar-0123abcd.rlib` (with or without the extension) -> `foo_bar`
fn crate_of(rlib: &Path) -> String {
    let stem = rlib.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let stem = stem.strip_suffix(".rlib").unwrap_or(&stem);
    let stem = stem.strip_prefix("lib").unwrap_or(stem);
    stem.rsplit_once('-').map_or(stem, |(k, _)| k).to_string()
}

/// nm complains on stderr about each rlib's metadata member and exits
/// nonzero for it; only stdout matters.
fn nm_lines(cmd: &mut Command) -> Result<Vec<String>> {
    let out = cmd.output().context("running nm")?;
    Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect())
}

struct Decl {
    /// (name, type)
    params: Vec<(String, String)>,
    /// carries a `link_name = "__ape_shim_..."` under cfg rust_ape_shim
    redirected: bool,
}

/// Every `pub fn` of the libc crate's linux-musl files.
fn fn_signatures(root: &Path) -> Result<HashMap<String, Decl>> {
    let mut out: HashMap<String, Decl> = HashMap::new();
    let mut files: Vec<PathBuf> =
        ARCHES.iter().flat_map(|a| genshim::libc_search_paths(root, a)).collect();
    for sub in ["new/linux_uapi", "new/musl", "new/common"] {
        walk(&root.join("vendor/patches/libc/src").join(sub), "rs", &mut files)?;
    }
    let files: BTreeSet<PathBuf> = files.into_iter().collect();
    for f in files {
        let Ok(text) = fs::read_to_string(&f) else { continue };
        let mut rest = text.as_str();
        while let Some(i) = rest.find("pub fn ") {
            // attributes sit between the end of the previous item and here
            let before = &rest[..i];
            let item_start = before.rfind([';', '{', '}']).map_or(0, |k| k + 1);
            let attrs = &before[item_start..];
            let redirected = attrs.contains("rust_ape_shim") && attrs.contains("__ape_shim_");
            rest = &rest[i + 7..];
            let Some(open) = rest.find('(') else { break };
            let name = rest[..open].trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            // up to the matching paren; fn-pointer params nest
            let mut depth = 0;
            let mut end = None;
            for (j, c) in rest[open..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(open + j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(end) = end else { break };
            let mut params = Vec::new();
            let mut depth = 0;
            let mut cur = String::new();
            for c in rest[open + 1..end].chars().chain(std::iter::once(',')) {
                match c {
                    '(' | '<' | '[' => depth += 1,
                    ')' | '>' | ']' => depth -= 1,
                    _ => {}
                }
                if c == ',' && depth == 0 {
                    if let Some((n, t)) = cur.split_once(':') {
                        params.push((n.trim().to_string(), t.split_whitespace().collect::<Vec<_>>().join(" ")));
                    }
                    cur.clear();
                } else {
                    cur.push(c);
                }
            }
            // cfg_if! branches for other systems declare the same names; the
            // branch carrying the redirect is the one this target compiles
            match out.entry(name.to_string()) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if redirected && !e.get().redirected {
                        e.insert(Decl { params, redirected });
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(Decl { params, redirected });
                }
            }
        }
    }
    Ok(out)
}

/// Integer parameters that aren't plainly a descriptor, a size or an id,
/// and pointers to structs whose fields hold constants.
fn suspect_params(params: &[(String, String)]) -> Vec<String> {
    const INT_TYPES: &[&str] = &[
        "c_int", "c_uint", "c_long", "c_ulong", "c_short", "c_ushort", "i32", "u32", "i64", "u64",
        "Ioctl", "clockid_t", "idtype_t", "nl_item", "__rlimit_resource_t", "__priority_which_t",
    ];
    const NUMBERS: &[&str] = &[
        "fd", "fd1", "fd2", "fildes", "filedes", "oldfd", "newfd", "dirfd", "olddirfd", "newdirfd",
        "sockfd", "socket", "sock", "s", "out_fd", "in_fd", "epfd", "src", "dst", "duration", "len", "size", "count", "n",
        "nbyte", "nbytes", "num", "c", "status", "code", "nfds", "timeout", "seconds", "secs",
        "usec", "base", "iovcnt", "nmemb", "maxevents", "backlog", "uid", "gid", "pid", "pgid",
        "pgrp", "sid", "owner", "group", "incr", "cpu", "errnum", "ngroups", "setlen", "stream",
        "ch", "key", "value", "val", "offset", "rank", "prio", "priority", "idx", "index",
        "i", "nochdir", "noclose", "proj_id", "nelem", "net", "argc", "proto", "port", "loc",
        "suffixlen", "stayopen", "seed", "pshared", "shared", "errcode",
    ];
    const STRUCTS: &[&str] = &[
        "termios", "sigaction", "flock", "pollfd", "sockaddr", "msghdr", "mmsghdr", "addrinfo",
        "epoll_event", "sigevent", "stack_t", "siginfo_t", "itimerspec", "rlimit",
    ];
    let mut out = Vec::new();
    for (name, ty) in params {
        let bare = ty.rsplit("::").next().unwrap_or(ty);
        let last = ty.split_whitespace().last().unwrap_or("");
        let last = last.rsplit("::").next().unwrap_or(last);
        if INT_TYPES.contains(&bare) && !NUMBERS.contains(&name.as_str()) {
            out.push(format!("{name}: {bare}"));
        } else if ty.starts_with('*') && STRUCTS.contains(&last) {
            out.push(format!("{name}: *{last}"));
        }
    }
    out
}

/// Every function the families a constant belongs to list, or None when no
/// family claims it.
fn consumers_of<'a>(families: &'a [Family], name: &str) -> Option<Vec<&'a String>> {
    let mut out = Vec::new();
    let mut claimed = false;
    for f in families {
        if in_family(f, name) {
            claimed = true;
            out.extend(&f.consumers);
        }
    }
    claimed.then_some(out)
}

fn in_family(f: &Family, name: &str) -> bool {
    f.prefixes.iter().any(|p| name.starts_with(p.as_str()))
        && !f.except.iter().any(|p| name.starts_with(p.as_str()))
}

/// Families with at least one differing value, labelled by the names that
/// differ, with their consumer functions.
fn hot_families<'a>(families: &'a [Family], rows: &[ConstRow]) -> Vec<(String, &'a [String])> {
    let mut out = Vec::new();
    for family in families {
        let consumers = &family.consumers;
        let differing: Vec<&str> = rows
            .iter()
            .filter(|r| !r.diffs.is_empty())
            .filter(|r| in_family(family, &r.name))
            .map(|r| r.name.as_str())
            .collect();
        if differing.is_empty() {
            continue;
        }
        let mut label = differing.iter().take(3).copied().collect::<Vec<_>>().join(",");
        if differing.len() > 3 {
            let _ = write!(label, ",+{}", differing.len() - 3);
        }
        out.push((label, consumers.as_slice()));
    }
    out
}

fn write_reports(
    work: &Path,
    rows: &[ConstRow],
    fn_rows: &[FnRow],
    imports: &BTreeMap<String, BTreeSet<String>>,
) -> Result<()> {
    let mut t = String::from("name\tverdict\tkind\tcosmo\tdiffers\tused by\n");
    for r in rows {
        let _ = writeln!(
            t,
            "{}\t{}\t{}\t{}\t{}\t{}",
            r.name,
            r.verdict,
            r.kind,
            r.cosmo_cell.trim(),
            r.diffs.join("; "),
            r.users
        );
    }
    fs::write(work.join("consts.tsv"), t)?;

    let flagged: HashMap<&str, &FnRow> = fn_rows.iter().map(|r| (r.name.as_str(), r)).collect();
    let mut t = String::from("function\tstatus\tfamilies\timported by\n");
    for (func, crates) in imports {
        let crates = crates.iter().cloned().collect::<Vec<_>>().join(" ");
        let (status, fam) = if func.starts_with("__ape_shim_") {
            ("shim", "")
        } else if let Some(r) = flagged.get(func.as_str()) {
            (r.verdict, r.families.as_str())
        } else {
            ("direct", "")
        };
        let _ = writeln!(t, "{func}\t{status}\t{fam}\t{crates}");
    }
    for r in fn_rows.iter().filter(|r| r.verdict.starts_with("unused")) {
        let _ = writeln!(t, "{}\t{}\t{}\t", r.name, r.verdict, r.families);
    }
    fs::write(work.join("functions.tsv"), t)?;
    Ok(())
}
