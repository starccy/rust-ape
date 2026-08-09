use crate::config::{self, PATCHED_CRATES, TARGETS};
use crate::metadata::{self, Metadata};
use crate::util;
use anyhow::{Context, Result, bail};
use clap::Args;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Extra flags handed to every C dependency.
///
/// `_GNU_SOURCE` puts cosmo's headers in the Linux shape a library built for
/// linux-musl expects, gating declarations like `madvise`. cosmo uses the
/// macro for visibility only, never to switch a function's signature the way
/// glibc does for `strerror_r`.
///
/// `scripts/cdeps-predef.h` rides along as a force-include for the cases a
/// `-D` cannot express; read it for what is in there and why.
///
/// The rest are self-referential macros for cosmo constants that are
/// `extern const int` rather than macros, so that a library's `#ifndef X`
/// probe answers "defined" at any include order instead of installing a
/// `#define X 0` fallback that breaks cosmo's declaration (libgit2's
/// src/util/posix.h). Uses still reach the runtime variable: GCC expands the
/// macro once. jemalloc needs `MADV_FREE`, where the failing probe is an
/// `#ifdef` that falls through to a name only its Linux configure path
/// defines, so the miss surfaces as an undeclared identifier in pages.c.
/// `HAVE_ENDIAN_H` is a different shape again: mikepb's portable/endian.h,
/// vendored by tree-sitter among others, picks its branch off `__linux__` and
/// friends and ends in `#error platform not supported` when none match. That
/// macro is the escape hatch it publishes, and cosmo's <endian.h> has every
/// name it would otherwise define.
const C_PREDEFS: &[&str] = &[
    "-D_GNU_SOURCE",
    "-DSOCK_CLOEXEC=SOCK_CLOEXEC",
    "-DMADV_FREE=MADV_FREE",
    "-DHAVE_ENDIAN_H",
];

#[derive(Args)]
pub struct BuildArgs {
    /// The project directory to build
    #[arg(value_name = "PROJECT_DIR")]
    pub project: PathBuf,
    #[arg(long)]
    pub release: bool,
    /// Force a std/libc rebuild (useful after hand-editing vendor/library)
    #[arg(long)]
    pub clean_std: bool,
    /// Build only this package of the project's workspace
    #[arg(long, short = 'p')]
    pub package: Option<String>,
    /// Build and pack only this binary
    #[arg(long)]
    pub bin: Option<String>,
    /// Build and pack only this example
    #[arg(long)]
    pub example: Option<String>,
    /// Space or comma separated list of features to activate
    #[arg(long, short = 'F')]
    pub features: Vec<String>,
    /// Activate all available features
    #[arg(long)]
    pub all_features: bool,
    /// Do not activate the `default` feature
    #[arg(long)]
    pub no_default_features: bool,
    /// Where to write the result (defaults to <project>/target/ape/<name>.com)
    #[arg(long)]
    pub output: Option<PathBuf>,
}

pub fn run(args: &BuildArgs) -> Result<()> {
    let root = util::repo_root();
    let project = args
        .project
        .canonicalize()
        .with_context(|| format!("no such project directory: {}", args.project.display()))?;
    preflight(&root)?;

    let meta = metadata::load(&project)?;
    verify_patches(&root, &meta)?;

    let targets = pack_targets(&meta, args)?;
    if args.output.is_some() && targets.len() > 1 {
        bail!(
            "--output names one file but {} binaries are being built ({}); pass --bin to pick one",
            targets.len(),
            targets.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
        );
    }
    let profile = if args.release { "release" } else { "debug" };

    // cargo treats build-std's std and libc as non-local and won't notice their
    // sources changing, so compare the SDK's stamp against what this project
    // last built with and wipe the cache when they disagree.
    let stamp = std_stamp(&root)?;
    let marker = project.join("target/ape/.std-stamp");
    let stale = fs::read_to_string(&marker)
        .map(|s| s.trim() != stamp)
        .unwrap_or(true);
    if args.clean_std || stale {
        if stale && !args.clean_std {
            println!("==> SDK std/libc changed, clearing the build-std cache");
        }
        clean_std(&project, profile)?;
    }

    purge_stale_shim_links(&root, &project, profile)?;

    for &(triple, arch) in TARGETS {
        println!("==> building {triple} ({profile})");
        cargo_build(&root, &project, triple, arch, args)?;
    }

    for target in &targets {
        let output = match &args.output {
            Some(o) => o.clone(),
            // let example binaries land in target/ape/examples/ so
            // they don't collide with the main bin
            None => {
                let mut dir = project.join("target/ape");
                if target.example {
                    dir = dir.join("examples");
                }
                dir.join(format!("{}.com", target.name))
            }
        };
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        apelink(&root, &project, profile, target, &output)?;
        println!("==> {} ({} bytes)", output.display(), fs::metadata(&output)?.len());
    }

    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&marker, format!("{stamp}\n"))?;
    Ok(())
}

fn preflight(root: &Path) -> Result<()> {
    let mut missing = Vec::new();
    for &(triple, _) in TARGETS {
        let rel = format!("generated/{triple}.json");
        if !root.join(&rel).is_file() {
            missing.push(rel);
        }
    }
    for rel in ["vendor/library", "vendor/cosmocc/bin"] {
        if !root.join(rel).is_dir() {
            missing.push(rel.to_string());
        }
    }
    if !missing.is_empty() {
        bail!(
            "SDK is not ready (missing {}). Run `cargo xtask setup` first",
            missing.join(", ")
        );
    }
    Ok(())
}

// Fails if a patched crate resolved to crates.io instead of vendor/patches,
// which happens when the registry gets a newer version in the same compat range.
fn verify_patches(root: &Path, meta: &Metadata) -> Result<()> {
    let mut bad = Vec::new();
    for &(name, ver) in PATCHED_CRATES {
        for p in &meta.packages {
            if p.name == name && p.from_registry() && compat_range(&p.version) == compat_range(ver) {
                bad.push((name, ver, p.version.as_str()));
            }
        }
    }
    if bad.is_empty() {
        return Ok(());
    }
    let mut msg =
        String::from("these crates still resolve to crates.io, so their patches are inert:\n");
    for (name, ver, got) in &bad {
        msg += &format!("  {name} {got} came from the registry, not vendor/patches (patch targets {ver})\n");
    }
    msg += "to fix:\n";
    for (name, ver, _) in &bad {
        msg += &format!(
            "  1) check the project's Cargo.toml has, under [patch.crates-io], {name} = {{ path = \"{}\" }}\n  2) run `cargo update -p {name} --precise {ver}` in the project\n",
            root.join("vendor/patches").join(name).display()
        );
    }
    bail!(msg);
}

/// The leading part of a version that cargo treats as one compatible range:
/// the major, or major.minor while the major is still 0.
fn compat_range(version: &str) -> &str {
    let mut end = version.find('.').unwrap_or(version.len());
    if &version[..end] == "0" {
        end = version[end + 1..]
            .find('.')
            .map_or(version.len(), |i| end + 1 + i);
    }
    &version[..end]
}

/// One executable to apelink: a bin, or an example (whose `.com.dbg` lands
/// in the profile dir's examples/ subdirectory).
struct PackTarget {
    name: String,
    example: bool,
}

fn pack_targets(meta: &Metadata, args: &BuildArgs) -> Result<Vec<PackTarget>> {
    let mut picked = Vec::new();
    if let Some(name) = &args.example {
        picked.push(PackTarget { name: name.clone(), example: true });
    }
    if let Some(name) = &args.bin {
        picked.push(PackTarget { name: name.clone(), example: false });
    }
    if !picked.is_empty() {
        return Ok(picked);
    }
    let pkg = match args.package.as_deref() {
        // -p limits the search to the workspace's own members, so a
        // dependency that happens to share the name can't be picked up.
        Some(name) => meta
            .packages
            .iter()
            .filter(|p| meta.workspace_members.contains(&p.id))
            .find(|p| p.name == name)
            .with_context(|| format!("no package named {name} in this workspace"))?,
        None => {
            let root_id = meta.resolve.root.as_deref().context(
                "could not tell which package is the root (virtual workspace?); pass -p or --bin",
            )?;
            meta.packages
                .iter()
                .find(|p| p.id == root_id)
                .context("the root package is missing from packages")?
        }
    };
    // if none was picked, build all the bins in the package by default
    let bins: Vec<PackTarget> = pkg
        .targets
        .iter()
        .filter(|t| t.kind.iter().any(|k| k == "bin"))
        .map(|t| PackTarget { name: t.name.clone(), example: false })
        .collect();
    if bins.is_empty() {
        bail!("{} has no binaries to build", pkg.name);
    }
    Ok(bins)
}

/// Identity of everything std is built from: the library and libc stamps.
fn std_stamp(root: &Path) -> Result<String> {
    let mut s = String::new();
    for c in ["library", "libc"] {
        let f = root.join("vendor/.stamps").join(c);
        s += fs::read_to_string(&f)
            .with_context(|| {
                format!("missing {}: run `cargo xtask setup` first", f.display())
            })?
            .trim();
        s.push('\n');
    }
    Ok(s.trim().to_string())
}

fn purge_stale_shim_links(root: &Path, project: &Path, profile: &str) -> Result<()> {
    let mut newest = None;
    for entry in fs::read_dir(root.join("shim"))? {
        let m = entry?.metadata()?.modified()?;
        if newest.is_none_or(|n| m > n) {
            newest = Some(m);
        }
    }
    let Some(newest) = newest else { return Ok(()) };
    for &(triple, _) in TARGETS {
        purge_older_linked(&project.join("target").join(triple).join(profile), newest)?;
    }
    Ok(())
}

fn purge_older_linked(dir: &Path, newer_than: std::time::SystemTime) -> Result<()> {
    let Ok(rd) = fs::read_dir(dir) else { return Ok(()) };
    for entry in rd {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            purge_older_linked(&path, newer_than)?;
        } else if path.to_string_lossy().ends_with(".com.dbg")
            && entry.metadata()?.modified()? <= newer_than
        {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn clean_std(project: &Path, profile: &str) -> Result<()> {
    for &(triple, _) in TARGETS {
        let base = project.join("target").join(triple).join(profile);
        
        remove_prefixed(&base.join(".fingerprint"), &["std-", "panic_abort-", "libc-"])?;
        remove_prefixed(&base.join("deps"), &["liblibc-", "libc-"])?;
    }
    Ok(())
}

fn remove_prefixed(dir: &Path, prefixes: &[&str]) -> Result<()> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in rd {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if prefixes.iter().any(|p| name.starts_with(p)) {
            let path = entry.path();
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

/// set key when it's not already set
fn env_default(cmd: &mut Command, key: &str, value: impl AsRef<std::ffi::OsStr>) {
    if std::env::var_os(key).is_none() {
        cmd.env(key, value);
    }
}

fn cargo_build(
    root: &Path,
    project: &Path,
    triple: &str,
    arch: &str,
    args: &BuildArgs,
) -> Result<()> {
    let target_json = root.join("generated").join(format!("{triple}.json"));
    let cosmo_bin = root.join("vendor/cosmocc/bin");
    let t = triple.replace('-', "_");

    let mut cmd = Command::new("cargo");
    cmd.current_dir(project);
    cmd.env("RUSTUP_TOOLCHAIN", config::rust_channel()?);
    // Where std's sources live; cargo wants this absolute.
    cmd.env("__CARGO_TESTS_ONLY_SRC_ROOT", root.join("vendor/library"));

    // * rustix_use_libc: keep rustix off its raw-syscall backend, which cosmo
    // can't translate away from Linux.
    //
    // * polling_test_poll_backend: poll() for async-io/smol. Now that
    // shim/epoll.c exists this looks removable, and it isn't: polling's epoll
    // backend arms its timeouts with timerfd_create/timerfd_settime, which
    // cosmo numbers but never wrapped, so dropping the cfg fails at link. It
    // would also need EPOLLONESHOT, which the emulation doesn't implement and
    // polling's whole API is built on. The poll() path costs nothing here --
    // shim/poll.c carries the same oversized-array retry that shim/epoll.c
    // does, so smol doesn't run into the NT ceiling either.
    //
    // * mio_unsupported_force_waker_pipe: mio's default waker
    // on Linux is an eventfd, which cosmo doesn't have.
    //
    // Passed as env because RUSTFLAGS there outranks anything
    // in a project's .cargo/config.toml, so nobody can drop these by
    // accident.
    let mut rustflags = std::env::var("RUSTFLAGS").unwrap_or_default();
    if !rustflags.is_empty() {
        rustflags.push(' ');
    }
    // rust_ape_shim: arms the __ape_shim_* redirects in the patched crates.
    // With --target set, RUSTFLAGS reaches target units only, so host build
    // scripts keep linking the real libc symbols.
    rustflags.push_str(
        "--cfg rustix_use_libc --cfg polling_test_poll_backend \
         --cfg mio_unsupported_force_waker_pipe --cfg rust_ape_shim",
    );
    cmd.env("RUSTFLAGS", rustflags);

    // C, C++ and asm in dependencies go through cosmocc too, so the ABI matches
    // what they'll run against.
    env_default(&mut cmd, &format!("CC_{t}"), cosmo_bin.join(format!("{arch}-unknown-cosmo-cc")));
    env_default(&mut cmd, &format!("CXX_{t}"), cosmo_bin.join(format!("{arch}-unknown-cosmo-c++")));
    env_default(
        &mut cmd,
        &format!("AR_{t}"),
        root.join("generated").join(format!("ar-{arch}.bash")),
    );
    for var in ["CFLAGS", "CXXFLAGS"] {
        let user = std::env::var(format!("{var}_{t}")).unwrap_or_default();
        // guard=global keeps things working when a -sys crate forces
        // -fstack-protector back on
        cmd.env(
            format!("{var}_{t}"),
            format!(
                "-fno-stack-protector -mstack-protector-guard=global {} -include {} {user}",
                C_PREDEFS.join(" "),
                root.join("scripts/cdeps-predef.h").display()
            ),
        );
    }

    cmd.args([
        "build",
        "-Z",
        "json-target-spec",
        "-Z",
        "build-std=std,panic_abort,panic_unwind",
        "-Z",
        "build-std-features=",
    ]);
    cmd.arg("--target").arg(&target_json);
    if args.release {
        cmd.arg("--release");
    }
    if let Some(p) = &args.package {
        cmd.args(["--package", p]);
    }
    if let Some(b) = &args.bin {
        cmd.args(["--bin", b]);
    }
    if let Some(e) = &args.example {
        cmd.args(["--example", e]);
    }
    for f in &args.features {
        cmd.args(["--features", f]);
    }
    if args.all_features {
        cmd.arg("--all-features");
    }
    if args.no_default_features {
        cmd.arg("--no-default-features");
    }
    util::run(&mut cmd)
}

fn apelink(
    root: &Path,
    project: &Path,
    profile: &str,
    target: &PackTarget,
    output: &Path,
) -> Result<()> {
    let bin = root.join("vendor/cosmocc/bin");
    let dbg = |triple: &str| {
        let mut dir = project.join("target").join(triple).join(profile);
        if target.example {
            dir = dir.join("examples");
        }
        dir.join(format!("{}.com.dbg", target.name))
    };
    let inputs: Vec<PathBuf> = TARGETS.iter().map(|&(triple, _)| dbg(triple)).collect();
    for p in &inputs {
        if !p.is_file() {
            bail!("{} not found. wrong binary name? pass --bin", p.display());
        }
    }

    println!("==> apelink -> {}", output.display());
    // -M ape-m1.c is the Apple Silicon bootstrap; some environments false-alarm
    // on it, so fall back to linking without it.
    let mut with_m1 = util::ape_command(&bin.join("apelink"));
    with_m1
        .arg("-l")
        .arg(bin.join("ape-x86_64.elf"))
        .arg("-l")
        .arg(bin.join("ape-aarch64.elf"))
        .arg("-M")
        .arg(bin.join("ape-m1.c"))
        .arg("-o")
        .arg(output)
        .args(&inputs);
    if !with_m1.status().context("could not run apelink")?.success() {
        eprintln!("warn: apelink -M failed; linking without Apple Silicon support");
        let mut plain = util::ape_command(&bin.join("apelink"));
        plain
            .arg("-l")
            .arg(bin.join("ape-x86_64.elf"))
            .arg("-l")
            .arg(bin.join("ape-aarch64.elf"))
            .arg("-o")
            .arg(output)
            .args(&inputs);
        util::run(&mut plain)?;
    }

    let pecheck = bin.join("pecheck");
    if pecheck.is_file() {
        util::run(util::ape_command(&pecheck).arg(output))?;
    }
    Ok(())
}
