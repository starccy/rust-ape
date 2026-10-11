pub mod bundle;
mod std_cache;
pub mod subsystem;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use self::std_cache::{
    clean_std, purge_older_linked, purge_stale_shim_links, purge_windows_macros, std_stamp,
    verify_std_redirects,
};
use crate::host::link;
use crate::metadata::{self, Metadata};
use crate::toolchain::config::{self, PATCHED_CRATES, TARGETS};
use crate::util;

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

/// What the command was asked to do; cli/build.rs has the flags.
pub struct Options {
    /// The project directory to build
    pub project: PathBuf,
    pub release: bool,
    /// Link cosmocc's tiny runtime (-mtiny), which drops the built-in
    /// --strace/--ftrace machinery in exchange for a smaller binary
    pub tiny: bool,
    /// Force a std/libc rebuild (useful after hand-editing vendor/library)
    pub clean_std: bool,
    /// Build only this package of the project's workspace
    pub package: Option<String>,
    /// Build and pack only this binary
    pub bin: Option<String>,
    /// Build and pack only this example
    pub example: Option<String>,
    /// Space or comma separated list of features to activate
    pub features: Vec<String>,
    /// Activate all available features
    pub all_features: bool,
    /// Do not activate the `default` feature
    pub no_default_features: bool,
    /// Where to write the result (defaults to <target dir>/ape/<profile>/<name>.com)
    pub output: Option<PathBuf>,
}

pub fn run(args: &Options) -> Result<()> {
    let root = util::repo_root();
    let project = args
        .project
        .canonicalize()
        .with_context(|| format!("no such project directory: {}", args.project.display()))?;
    preflight(&root)?;
    config::check_std_libc()?;

    let meta = metadata::load(&project)?;
    // cargo's target directory, the workspace's when the project is a member
    let target_dir = meta.target_directory.clone();
    verify_patches(&root, &meta)?;

    let targets = pack_targets(&meta, args)?;
    // validated up front so a bad bundle.toml fails before the long build
    let bundle_spec = bundle::load(&project)?;
    if args.output.is_some() && targets.len() > 1 {
        bail!(
            "--output names one file but {} binaries are being built ({}); pass --bin to pick one",
            targets.len(),
            targets
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let profile = if args.release { "release" } else { "debug" };

    // cargo treats build-std's std and libc as non-local and won't notice their
    // sources changing, so compare the SDK's stamp against what this project
    // last built with and wipe the cache when they disagree.
    let stamp = std_stamp(&root)?;
    let marker = target_dir.join("ape/.std-stamp");
    let stale = fs::read_to_string(&marker)
        .map(|s| s.trim() != stamp)
        .unwrap_or(true);
    if args.clean_std || stale {
        if stale && !args.clean_std {
            println!("==> SDK std/libc changed, clearing the build-std cache");
        }
        clean_std(&target_dir, profile)?;
    }

    purge_stale_shim_links(&root, &target_dir, profile)?;

    let host_libs = link::names_from_metadata(&meta)?;
    let crate_modes = link::crate_modes_from_metadata(&meta)?;
    // what [package.metadata.rust-ape.host-libs] says reaches the link,
    // which cargo doesn't redo when it changes (the crates it reaches
    // are rebuilt through the cfg below)
    let host_libs_stamp = target_dir.join("ape").join(format!(".host-libs-{profile}"));
    if fs::read_to_string(&host_libs_stamp).ok().as_deref() != Some(host_libs.as_str()) {
        for &(triple, _) in TARGETS {
            purge_older_linked(
                &target_dir.join(triple).join(profile),
                std::time::SystemTime::now(),
            )?;
        }
    }
    purge_windows_macros(&target_dir, profile)?;
    for &(triple, arch) in TARGETS {
        println!("==> building {triple} ({profile})");
        cargo_build(
            &root,
            &project,
            triple,
            arch,
            args,
            &host_libs,
            &crate_modes,
        )?;
        verify_std_redirects(&target_dir, profile, triple)?;
    }

    for target in &targets {
        let output = match &args.output {
            Some(o) => o.clone(),
            // let example binaries land in target/ape/<profile>/examples/
            // so they don't collide with the main bin
            None => {
                let mut dir = target_dir.join("ape").join(profile);
                if target.example {
                    dir = dir.join("examples");
                }
                dir.join(format!("{}.com", target.name))
            }
        };
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        apelink(&root, &target_dir, profile, target, &output, args.tiny)?;
        if let Some(spec) = &bundle_spec {
            let dbg = dbg_path(&target_dir, profile, target, TARGETS[0].0);
            bundle::apply_to_build(spec, &dbg, &output, &target.name)?;
        }
        if let Some(src) = &target.src {
            if let Some(sub) = subsystem::from_source(src, !args.release)? {
                subsystem::apply(&output, sub)?;
                println!("==> windows subsystem: {sub:?}");
            }
        }
        println!(
            "==> {} ({} bytes)",
            output.display(),
            fs::metadata(&output)?.len()
        );
    }

    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&marker, format!("{stamp}\n"))?;
    fs::write(&host_libs_stamp, &host_libs)?;
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
    if !root.join("vendor/cosmocc/bin").is_dir() {
        missing.push("vendor/cosmocc/bin".to_string());
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
            if p.name == name && p.from_registry() && compat_range(&p.version) == compat_range(ver)
            {
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
        msg += &format!(
            "  {name} {got} came from the registry, not vendor/patches (patch targets {ver})\n"
        );
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
    src: Option<PathBuf>,
}

fn pack_targets(meta: &Metadata, args: &Options) -> Result<Vec<PackTarget>> {
    let src = |name: &str, kind: &str| {
        meta.packages
            .iter()
            .filter(|p| meta.workspace_members.contains(&p.id))
            .flat_map(|p| &p.targets)
            .find(|t| t.name == name && t.kind.iter().any(|k| k == kind))
            .map(|t| t.src_path.clone())
    };
    let mut picked = Vec::new();
    if let Some(name) = &args.example {
        picked.push(PackTarget {
            name: name.clone(),
            example: true,
            src: src(name, "example"),
        });
    }
    if let Some(name) = &args.bin {
        picked.push(PackTarget {
            name: name.clone(),
            example: false,
            src: src(name, "bin"),
        });
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
        .map(|t| PackTarget {
            name: t.name.clone(),
            example: false,
            src: Some(t.src_path.clone()),
        })
        .collect();
    if bins.is_empty() {
        bail!("{} has no binaries to build", pkg.name);
    }
    Ok(bins)
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
    args: &Options,
    host_libs: &str,
    crate_modes: &str,
) -> Result<()> {
    let target_json = root.join("generated").join(format!("{triple}.json"));
    let cosmo_bin = root.join("vendor/cosmocc/bin");
    let t = triple.replace('-', "_");

    let mut cmd = Command::new("cargo");
    cmd.current_dir(project);
    cmd.env("RUSTUP_TOOLCHAIN", config::rust_channel()?);

    // * rustix_use_libc: keep rustix off its raw-syscall backend, which cosmo
    // can't translate away from Linux.
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
    rustflags.push_str("--cfg rustix_use_libc --cfg rust_ape_shim");
    // cargo doesn't rebuild a crate when RUSTC_WRAPPER changes what it does,
    // so a new version of the rewrite says so here
    rustflags.push_str(&format!(
        " --cfg rust_ape_hostlink_{}",
        crate::host::rustcwrap::VERSION
    ));
    // likewise when the project changes which crates are one host's code,
    // or which of its libraries exist on which host
    if crate_modes != "{}" || host_libs != "{}" {
        use sha2::Digest;
        let h = sha2::Sha256::digest(format!("{crate_modes}\0{host_libs}").as_bytes());
        rustflags.push_str(&format!(
            " --cfg rust_ape_crate_modes_{:02x}{:02x}{:02x}{:02x}",
            h[0], h[1], h[2], h[3]
        ));
    }
    // -mtiny reaches the final cosmocc invocation through the linker wrapper
    // and swaps in the lib/tiny runtime, which has no --strace/--ftrace.
    if args.tiny {
        rustflags.push_str(" -C link-arg=-mtiny");
    }
    cmd.env("RUSTFLAGS", rustflags);

    // the linker wrapper runs `xtask hostlink` for libraries only the host has
    cmd.env("RUST_APE_XTASK", std::env::current_exe()?);
    cmd.env("RUST_APE_HOST_LIBS", host_libs);
    cmd.env("RUST_APE_CRATE_MODES", crate_modes);
    // and rustc goes through xtask too, which rewrites the declarations
    // of such libraries into functions calling them the host's way; a
    // wrapper already set (sccache, say) still runs, after it
    if let Some(prev) = std::env::var_os("RUSTC_WRAPPER") {
        cmd.env(crate::host::rustcwrap::NEXT, prev);
    }
    cmd.env("RUSTC_WRAPPER", std::env::current_exe()?);
    cmd.env(crate::host::rustcwrap::ENV, "1");

    // C, C++ and asm in dependencies go through cosmocc too, so the ABI matches
    // what they'll run against.
    env_default(
        &mut cmd,
        &format!("CC_{t}"),
        root.join("generated").join(format!("cc-{arch}.bash")),
    );
    env_default(
        &mut cmd,
        &format!("CXX_{t}"),
        cosmo_bin.join(format!("{arch}-unknown-cosmo-c++")),
    );
    env_default(
        &mut cmd,
        &format!("AR_{t}"),
        root.join("generated").join(format!("ar-{arch}.bash")),
    );
    // for C deps, -mtiny also drops the ftrace NOP padding on function entries
    let tiny = if args.tiny { " -mtiny" } else { "" };
    for var in ["CFLAGS", "CXXFLAGS"] {
        let user = std::env::var(format!("{var}_{t}")).unwrap_or_default();
        // guard=global keeps things working when a -sys crate forces
        // -fstack-protector back on
        cmd.env(
            format!("{var}_{t}"),
            format!(
                "-fno-stack-protector -mstack-protector-guard=global{tiny} {} -include {} {user}",
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
    // std is built from the toolchain's own rust-src, unpatched. The one
    // thing it has to pick up is the cosmo-adapted libc, and a patch given
    // on the command line reaches the build-std graph where the project's
    // own [patch] table does not.
    cmd.arg("--config").arg(format!(
        "patch.crates-io.libc.path={:?}",
        root.join("vendor/patches/libc").display().to_string()
    ));
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

/// The per-arch ELF cargo produced, which apelink fuses into the fat APE.
fn dbg_path(target_dir: &Path, profile: &str, target: &PackTarget, triple: &str) -> PathBuf {
    let mut dir = target_dir.join(triple).join(profile);
    if target.example {
        dir = dir.join("examples");
    }
    dir.join(format!("{}.com.dbg", target.name))
}

fn apelink(
    root: &Path,
    target_dir: &Path,
    profile: &str,
    target: &PackTarget,
    output: &Path,
    tiny: bool,
) -> Result<()> {
    let bin = root.join("vendor/cosmocc/bin");
    let inputs: Vec<PathBuf> = TARGETS
        .iter()
        .map(|&(triple, _)| dbg_path(target_dir, profile, target, triple))
        .collect();
    for p in &inputs {
        if !p.is_file() {
            bail!("{} not found. wrong binary name? pass --bin", p.display());
        }
    }

    println!("==> apelink -> {}", output.display());
    // -M ape-m1.c is the Apple Silicon bootstrap; some environments false-alarm
    // on it, so fall back to linking without it.
    let mut with_m1 = util::ape_command(&bin.join("apelink"));
    // the tiny runtime has no backtrace printer to read .symtab.* back, so
    // embedding the symbol tables would be dead weight
    if tiny {
        with_m1.arg("-s");
    }
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
        if tiny {
            plain.arg("-s");
        }
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
