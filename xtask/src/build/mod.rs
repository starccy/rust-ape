pub mod bundle;
mod std_cache;
pub mod subsystem;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use self::std_cache::{
    clean_std, purge_older_linked, purge_proc_macros, purge_stale_shim_links, std_stamp,
    verify_std_redirects,
};
use crate::host::link;
use crate::metadata::{self, Metadata};
pub use crate::toolchain::config::TARGETS;
use crate::toolchain::config::{self, PATCHED_CRATES};
use crate::util;

/// Extra flags handed to every C dependency.
///
/// `_GNU_SOURCE` puts cosmo's headers in the Linux shape a library built for
/// linux-musl expects, gating declarations like `madvise`. The fork also
/// follows glibc where the macro switches a signature: `strerror_r` is the
/// GNU `char *` one under it (OpenSSL's o_str.c assumes exactly that), the
/// XSI one otherwise.
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
    let session = Session::open(&args.project, args.release, args.clean_std)?;
    let targets = pack_targets(&session.meta, args)?;
    // validated up front so a bad bundle.toml fails before the long build
    let bundle_spec = bundle::load(&session.project)?;
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
    let profile = session.profile;
    let target_dir = &session.target_dir;

    for &(triple, arch) in TARGETS {
        println!("==> building {triple} ({profile})");
        let mut cmd = session.cargo(triple, arch, "build", args.tiny)?;
        cmd.args(selection_args(
            args.package.as_deref(),
            &args.features,
            args.all_features,
            args.no_default_features,
        ));
        if let Some(b) = &args.bin {
            cmd.args(["--bin", b]);
        }
        if let Some(e) = &args.example {
            cmd.args(["--example", e]);
        }
        util::run(&mut cmd)?;
        verify_std_redirects(target_dir, profile, triple)?;
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
        let inputs: Vec<PathBuf> = TARGETS
            .iter()
            .map(|&(triple, _)| dbg_path(target_dir, profile, target, triple))
            .collect();
        for p in &inputs {
            if !p.is_file() {
                bail!("{} not found. wrong binary name? pass --bin", p.display());
            }
        }
        apelink(&session.root, &inputs, &output, args.tiny)?;
        if let Some(spec) = &bundle_spec {
            bundle::apply_to_build(spec, &inputs[0], &output, &target.name)?;
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
    session.finish()
}

/// A project about to be built for both targets: the checks and cache
/// upkeep done once before cargo runs, and what every cargo run needs
pub struct Session {
    pub root: PathBuf,
    pub project: PathBuf,
    pub meta: Metadata,
    /// cargo's target directory, the workspace's when the project is a member
    pub target_dir: PathBuf,
    /// "debug" or "release"
    pub profile: &'static str,
    host_libs: String,
    crate_modes: String,
    std_stamp: String,
}

impl Session {
    pub fn open(project: &Path, release: bool, clean: bool) -> Result<Session> {
        let root = util::repo_root();
        let project = project
            .canonicalize()
            .with_context(|| format!("no such project directory: {}", project.display()))?;
        preflight(&root)?;
        config::check_std_libc()?;

        let meta = metadata::load(&project)?;
        let target_dir = meta.target_directory.clone();
        verify_patches(&root, &meta)?;
        let profile = if release { "release" } else { "debug" };

        // cargo treats build-std's std and libc as non-local and won't notice their
        // sources changing, so compare the SDK's stamp against what this project
        // last built with and wipe the cache when they disagree.
        let std_stamp = std_stamp(&root)?;
        let stale = fs::read_to_string(target_dir.join("ape/.std-stamp"))
            .map(|s| s.trim() != std_stamp)
            .unwrap_or(true);
        if clean || stale {
            if stale && !clean {
                println!("==> SDK std/libc changed, clearing the build-std cache");
            }
            clean_std(&target_dir, profile)?;
        }

        purge_stale_shim_links(&root, &target_dir, profile)?;

        let host_libs = link::names_from_metadata(&meta)?;
        let crate_modes = link::crate_modes_from_metadata(&meta)?;
        // what [package.metadata.rust-ape.host-libs] says reaches the link,
        // which cargo doesn't redo when it changes (the crates it reaches
        // are rebuilt through the cfg in cargo())
        let session = Session {
            root,
            project,
            meta,
            target_dir,
            profile,
            host_libs,
            crate_modes,
            std_stamp,
        };
        if fs::read_to_string(session.host_libs_stamp())
            .ok()
            .as_deref()
            != Some(session.host_libs.as_str())
        {
            for &(triple, _) in TARGETS {
                purge_older_linked(
                    &session.target_dir.join(triple).join(profile),
                    std::time::SystemTime::now(),
                )?;
            }
        }
        purge_proc_macros(&session.target_dir, profile)?;
        Ok(session)
    }

    fn host_libs_stamp(&self) -> PathBuf {
        self.target_dir
            .join("ape")
            .join(format!(".host-libs-{}", self.profile))
    }

    /// Records what this build used, for the next session's checks
    pub fn finish(&self) -> Result<()> {
        let dir = self.target_dir.join("ape");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(".std-stamp"), format!("{}\n", self.std_stamp))?;
        fs::write(self.host_libs_stamp(), &self.host_libs)?;
        Ok(())
    }
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

impl Session {
    /// `cargo <sub>` for one target, with the toolchain, wrappers and flags
    /// set; the caller adds what to build
    pub fn cargo(&self, triple: &str, arch: &str, sub: &str, tiny: bool) -> Result<Command> {
        let root = &self.root;
        let (host_libs, crate_modes) = (self.host_libs.as_str(), self.crate_modes.as_str());
        let target_json = root.join("generated").join(format!("{triple}.json"));
        let cosmo_bin = root.join("vendor/cosmocc/bin");
        let t = triple.replace('-', "_");

        let mut cmd = Command::new("cargo");
        cmd.current_dir(&self.project);
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
        // so a hash of the rewrite's source goes in here
        rustflags.push_str(&format!(
            " --cfg rust_ape_hostlink_{}",
            crate::host::rustcwrap::REWRITE_HASH
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
        if tiny {
            rustflags.push_str(" -C link-arg=-mtiny");
        }
        rustflags.push_str(&crate::toolchain::tls_plugin::rustflags(
            root,
            &target_json,
        )?);
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
        let tiny = if tiny { " -mtiny" } else { "" };
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
            sub,
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
        if self.profile == "release" {
            cmd.arg("--release");
        }
        Ok(cmd)
    }
}

/// The flags that pick packages and features, the same for build and test
pub fn selection_args(
    package: Option<&str>,
    features: &[String],
    all_features: bool,
    no_default_features: bool,
) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(p) = package {
        v.extend(["--package".into(), p.into()]);
    }
    for f in features {
        v.extend(["--features".into(), f.clone()]);
    }
    if all_features {
        v.push("--all-features".into());
    }
    if no_default_features {
        v.push("--no-default-features".into());
    }
    v
}

/// The per-arch ELF cargo produced, which apelink fuses into the fat APE.
fn dbg_path(target_dir: &Path, profile: &str, target: &PackTarget, triple: &str) -> PathBuf {
    let mut dir = target_dir.join(triple).join(profile);
    if target.example {
        dir = dir.join("examples");
    }
    dir.join(format!("{}.com.dbg", target.name))
}

/// Fuses the per-arch ELFs, in TARGETS order, into the APE at output
pub fn apelink(root: &Path, inputs: &[PathBuf], output: &Path, tiny: bool) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let bin = root.join("vendor/cosmocc/bin");

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
        .args(inputs);
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
            .args(inputs);
        util::run(&mut plain)?;
    }

    let pecheck = bin.join("pecheck");
    if pecheck.is_file() {
        util::run(util::ape_command(&pecheck).arg(output))?;
    }
    Ok(())
}
