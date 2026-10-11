//! `cargo xtask bundle`: pack local files into an APE's zip store so that
//! Linux-only data paths keep working on hosts that lack them.
//!
//! The project root's `bundle.toml` lists what goes in:
//!
//! ```toml
//! [[bundle]]
//! source = "/usr/share/zoneinfo"          # a file or a directory on this machine
//! paths = ["/usr/share/zoneinfo"]         # the Linux paths libraries read
//!
//! [[bundle]]
//! source = "/etc/ssl/certs"
//! paths = ["/etc/ssl/certs", "/etc/pki/tls/certs"]
//! bins = ["server"]                       # optional: only these binaries
//! ```
//!
//! The first path decides where the data lives inside the zip: under
//! `/zip/.bundle` plus that path. The rest become aliases of it. A relative
//! `source` is resolved against the project root. Without `bins` an entry
//! goes into every binary the project produces.
//!
//! The `.bundle` subtree keeps the data clear of cosmo's own zip assets;
//! libc.a carries a small `usr/share/zoneinfo` of its own, linked in by
//! `localtime_r`, and appending a second copy of those names corrupts the
//! archive.
//!
//! Alongside the data goes `.bundle-info`, one `linux-path=/zip/path` line
//! per alias. That file is the contract with cosmo's zipos-remap.c (in the
//! fork), which reads it on the first lookup and rewrites lookups of those
//! paths into the zip whenever the host has nothing there. The toml never
//! enters the binary.
//!
//! `cargo xtask build` runs this on every binary it links when a
//! `bundle.toml` is present, so a rebuild never loses the bundle. The
//! subcommand exists for applying one to a binary built earlier.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

pub const MANIFEST: &str = ".bundle-info";
const ROOT: &str = ".bundle";

/// What the command was asked to do; cli/bundle.rs has the flags.
pub struct Options {
    /// The APE binary to append the bundle to
    pub binary: PathBuf,
    /// Project directory holding bundle.toml (default: the current directory)
    pub project: Option<PathBuf>,
    /// Binary name to match `bins` against (default: the file name minus .com)
    pub bin: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleFile {
    #[serde(default)]
    bundle: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    source: PathBuf,
    paths: Vec<String>,
    #[serde(default)]
    bins: Vec<String>,
}

pub fn run(args: &Options) -> Result<()> {
    let project = match &args.project {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };
    let Some(spec) = load(&project)? else {
        bail!("no bundle.toml in {}", project.display());
    };
    let name = match &args.bin {
        Some(n) => n.clone(),
        None => {
            let stem = args
                .binary
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
            stem.trim_end_matches(".com").to_string()
        }
    };
    apply(&spec, &args.binary, &name)
}

/// The validated contents of a bundle.toml.
pub struct Spec {
    entries: Vec<Entry>,
}

/// Reads and validates `<project>/bundle.toml`. `Ok(None)` when there is none.
pub fn load(project: &Path) -> Result<Option<Spec>> {
    let path = project.join("bundle.toml");
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    let file: BundleFile =
        toml::from_str(&text).with_context(|| format!("{}: bad bundle.toml", path.display()))?;
    let mut entries = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for mut e in file.bundle {
        if e.source.is_relative() {
            e.source = project.join(&e.source);
        }
        if !e.source.exists() {
            bail!("bundle.toml: source {} does not exist", e.source.display());
        }
        if e.paths.is_empty() {
            bail!("bundle.toml: {} has no paths", e.source.display());
        }
        for p in &mut e.paths {
            let norm = normalize(p);
            if norm.len() < 2 {
                bail!(
                    "bundle.toml: {p:?} is not a usable Linux path (needs an absolute path below /)"
                );
            }
            if seen.contains(&norm) {
                bail!("bundle.toml: {norm} is listed twice");
            }
            seen.push(norm.clone());
            *p = norm;
        }
        entries.push(e);
    }
    Ok(Some(Spec { entries }))
}

/// Lexical cleanup to the shape zipos-remap.c matches against: absolute, no
/// `.`/`..`, no doubled or trailing slashes.
fn normalize(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Appends the entries that apply to `bin_name` to `binary`. Refuses a
/// binary that already has a bundle.
pub fn apply(spec: &Spec, binary: &Path, bin_name: &str) -> Result<()> {
    let entries: Vec<&Entry> = spec
        .entries
        .iter()
        .filter(|e| e.bins.is_empty() || e.bins.iter().any(|b| b == bin_name))
        .collect();
    if entries.is_empty() {
        return Ok(());
    }
    {
        let f = File::open(binary).with_context(|| format!("open {}", binary.display()))?;
        let archive = ZipArchive::new(f)
            .with_context(|| format!("{} is not a zip-capable APE binary", binary.display()))?;
        if archive.file_names().any(|n| n == MANIFEST) {
            bail!(
                "{} already carries a bundle; rebuild it before bundling again",
                binary.display()
            );
        }
    }

    // appended on a copy and moved into place, so a failure halfway leaves
    // the original untouched
    let name = binary.file_name().unwrap_or_default().to_string_lossy();
    let tmp = binary.with_file_name(format!("{name}.bundle-tmp"));
    fs::copy(binary, &tmp).with_context(|| format!("copy {}", binary.display()))?;
    let result = append(&entries, &tmp);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    fs::rename(&tmp, binary)?;
    Ok(())
}

fn append(entries: &[&Entry], binary: &Path) -> Result<()> {
    let f = fs::OpenOptions::new().read(true).write(true).open(binary)?;
    let mut zip = ZipWriter::new_append(f)?;
    // fixed timestamps keep the output identical across runs
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(DateTime::default());
    let mut manifest = String::new();
    let mut members: Vec<String> = Vec::new();
    for e in entries {
        let member = format!("{ROOT}{}", e.paths[0]);
        println!("==> bundle {} -> /zip/{member}", e.source.display());
        add_tree(&mut zip, &e.source, &member, opts, &mut members)?;
        for p in &e.paths {
            manifest.push_str(&format!("{p}=/zip/{member}\n"));
        }
    }
    zip.start_file(MANIFEST, opts.unix_permissions(0o644))?;
    zip.write_all(manifest.as_bytes())?;
    let mut f = zip.finish()?;
    f.flush()?;
    drop(f);

    // read it back the way the binary will
    let archive = ZipArchive::new(File::open(binary)?)?;
    let missing: Vec<&String> = members
        .iter()
        .filter(|m| archive.index_for_name(m).is_none())
        .collect();
    if archive.index_for_name(MANIFEST).is_none() || !missing.is_empty() {
        bail!(
            "{}: the bundle did not survive the append ({} members missing); \
             the file is likely corrupt, rebuild it",
            binary.display(),
            missing.len() + usize::from(archive.index_for_name(MANIFEST).is_none())
        );
    }
    Ok(())
}

fn add_tree(
    zip: &mut ZipWriter<File>,
    src: &Path,
    member: &str,
    opts: SimpleFileOptions,
    members: &mut Vec<String>,
) -> Result<()> {
    // parent directories get explicit entries, the way `zip -r` writes them,
    // so a stat of /zip/usr or /zip/usr/share answers like a directory
    let mut prefix = String::new();
    for seg in member.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(seg);
        let is_leaf = prefix.len() == member.len();
        if is_leaf && !src.is_dir() {
            break;
        }
        ensure_dir(zip, &prefix, opts, members)?;
    }
    if src.is_dir() {
        let mut names: Vec<PathBuf> = fs::read_dir(src)?
            .map(|e| e.map(|e| e.path()))
            .collect::<io::Result<_>>()?;
        names.sort();
        for child in names {
            let name = child.file_name().unwrap().to_string_lossy().into_owned();
            add_tree(zip, &child, &format!("{member}/{name}"), opts, members)?;
        }
    } else {
        let mode = file_mode(src);
        zip.start_file(member, opts.unix_permissions(mode))?;
        let mut f = File::open(src).with_context(|| format!("read {}", src.display()))?;
        io::copy(&mut f, zip)?;
        members.push(member.to_string());
    }
    Ok(())
}

fn ensure_dir(
    zip: &mut ZipWriter<File>,
    dir: &str,
    opts: SimpleFileOptions,
    members: &mut Vec<String>,
) -> Result<()> {
    let key = format!("{dir}/");
    if members.contains(&key) {
        return Ok(());
    }
    zip.add_directory(dir, opts.unix_permissions(0o755))?;
    members.push(key);
    Ok(())
}

#[cfg(unix)]
fn file_mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o644)
}

#[cfg(not(unix))]
fn file_mode(_: &Path) -> u32 {
    0o644
}

/// Does the linked binary carry zipos? cosmo reads the manifest through
/// `/zip`, and a binary whose link dropped zipos would ignore the bundle
/// silently. The unstripped `.com.dbg` is searched for the symbol name.
pub fn check_zipos(dbg: &Path) -> Result<()> {
    let bytes = fs::read(dbg)?;
    let needle = b"__zipos_open";
    if !bytes.windows(needle.len()).any(|w| w == needle) {
        bail!(
            "{} was linked without zipos; the bundle would be unreachable. \
             The linker wrapper passes -u zipos in tiny mode, so this needs a look.",
            dbg.display()
        );
    }
    Ok(())
}

/// What build runs after apelink: the zipos check, then the append.
pub fn apply_to_build(spec: &Spec, dbg: &Path, output: &Path, bin_name: &str) -> Result<()> {
    check_zipos(dbg)?;
    apply(spec, output, bin_name)
}
