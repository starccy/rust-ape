use crate::config::{self, PATCHED_CRATES, TARGETS};
use crate::util;
use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Args)]
pub struct BuildArgs {
    /// The project to build.
    #[arg(long)]
    pub project: PathBuf,
    #[arg(long)]
    pub release: bool,
    /// Force a std/libc rebuild (useful after hand-editing vendor/library)
    #[arg(long)]
    pub clean_std: bool,
    /// Build and pack only this binary
    #[arg(long)]
    pub bin: Option<String>,
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

    let meta = cargo_metadata(&project)?;
    verify_patches(&root, &meta)?;

    let artifact = match &args.bin {
        Some(b) => b.clone(),
        None => root_package_name(&meta)?,
    };
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

    for &(triple, arch) in TARGETS {
        println!("==> building {triple} ({profile})");
        cargo_build(&root, &project, triple, arch, profile, args)?;
    }

    let output = match &args.output {
        Some(o) => o.clone(),
        None => project.join("target/ape").join(format!("{artifact}.com")),
    };
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    apelink(&root, &project, profile, &artifact, &output)?;

    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&marker, format!("{stamp}\n"))?;

    util::run(Command::new("ls").arg("-la").arg(&output))?;
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
            "SDK is not ready (missing {}) — run `cargo xtask setup` first",
            missing.join(", ")
        );
    }
    Ok(())
}

fn cargo_metadata(project: &Path) -> Result<Value> {
    let out = util::capture_with_stderr(
        Command::new("cargo")
            .current_dir(project)
            .env("RUSTUP_TOOLCHAIN", config::rust_channel()?)
            .args(["metadata", "--format-version", "1"]),
    )?;
    serde_json::from_str(&out).context("cargo metadata did not return valid JSON")
}

/// If a crate we must patch still resolves to crates.io (e.g. the registry
/// published a newer version so the patch entry no longer matches), stop
/// instead of producing a subtly wrong binary.
fn verify_patches(root: &Path, meta: &Value) -> Result<()> {
    let packages = meta["packages"]
        .as_array()
        .context("cargo metadata has no packages")?;
    let mut bad = Vec::new();
    for &(name, ver) in PATCHED_CRATES {
        for p in packages {
            if p["name"].as_str() == Some(name)
                && p["source"]
                    .as_str()
                    .is_some_and(|s| s.contains("crates.io"))
            {
                bad.push((name, ver, p["version"].as_str().unwrap_or("?").to_string()));
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

fn root_package_name(meta: &Value) -> Result<String> {
    let root_id = meta["resolve"]["root"]
        .as_str()
        .context("could not tell which package is the root (virtual workspace?); pass --bin")?;
    let packages = meta["packages"].as_array().context("no packages")?;
    packages
        .iter()
        .find(|p| p["id"] == root_id)
        .and_then(|p| p["name"].as_str())
        .map(str::to_owned)
        .context("the root package is missing from packages")
}

/// Identity of everything std is built from: the library and libc stamps.
fn std_stamp(root: &Path) -> Result<String> {
    let mut s = String::new();
    for c in ["library", "libc"] {
        let f = root.join("vendor/.stamps").join(c);
        s += fs::read_to_string(&f)
            .with_context(|| {
                format!("missing {} — run `cargo xtask setup` first", f.display())
            })?
            .trim();
        s.push('\n');
    }
    Ok(s.trim().to_string())
}

fn clean_std(project: &Path, profile: &str) -> Result<()> {
    for &(triple, _) in TARGETS {
        let base = project.join("target").join(triple).join(profile);
        // libc goes too — it's a separate unit, and a rebuilt std would just
        // relink the stale one.
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

/// Env vars set explicitly by the caller take precedence, so a different
/// cross-compiler can still be forced from outside if needed.
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
    profile: &str,
    args: &BuildArgs,
) -> Result<()> {
    let _ = profile;
    let target_json = root.join("generated").join(format!("{triple}.json"));
    let cosmo_bin = root.join("vendor/cosmocc/bin");
    let t = triple.replace('-', "_");

    let mut cmd = Command::new("cargo");
    cmd.current_dir(project);
    cmd.env("RUSTUP_TOOLCHAIN", config::rust_channel()?);
    // Where std's sources live; cargo wants this absolute.
    cmd.env("__CARGO_TESTS_ONLY_SRC_ROOT", root.join("vendor/library"));

    // rustix_use_libc: keep rustix off its raw-syscall backend, which cosmo
    // can't translate away from Linux. polling_test_poll_backend: poll(), since
    // there's no epoll. Passed as env because RUSTFLAGS there outranks anything
    // in a project's .cargo/config.toml, so nobody can drop these two by
    // accident.
    let mut rustflags = std::env::var("RUSTFLAGS").unwrap_or_default();
    if !rustflags.is_empty() {
        rustflags.push(' ');
    }
    rustflags.push_str("--cfg rustix_use_libc --cfg polling_test_poll_backend");
    cmd.env("RUSTFLAGS", rustflags);

    // C and asm in dependencies go through cosmocc too, so the ABI matches what
    // they'll run against. -fno-stack-protector: cosmo has no __stack_chk_guard.
    env_default(&mut cmd, &format!("CC_{t}"), cosmo_bin.join(format!("{arch}-unknown-cosmo-cc")));
    env_default(
        &mut cmd,
        &format!("AR_{t}"),
        root.join("generated").join(format!("ar-{arch}.bash")),
    );
    let user_cflags = std::env::var(format!("CFLAGS_{t}")).unwrap_or_default();
    cmd.env(format!("CFLAGS_{t}"), format!("-fno-stack-protector {user_cflags}"));

    cmd.args([
        "build",
        "-Z",
        "json-target-spec",
        "-Z",
        "build-std=panic_abort,std",
        "-Z",
        "build-std-features=",
    ]);
    cmd.arg("--target").arg(&target_json);
    if args.release {
        cmd.arg("--release");
    }
    if let Some(b) = &args.bin {
        cmd.args(["--bin", b]);
    }
    util::run(&mut cmd)
}

fn apelink(
    root: &Path,
    project: &Path,
    profile: &str,
    artifact: &str,
    output: &Path,
) -> Result<()> {
    let bin = root.join("vendor/cosmocc/bin");
    let dbg = |triple: &str| {
        project
            .join("target")
            .join(triple)
            .join(profile)
            .join(format!("{artifact}.com.dbg"))
    };
    let inputs: Vec<PathBuf> = TARGETS.iter().map(|&(triple, _)| dbg(triple)).collect();
    for p in &inputs {
        if !p.is_file() {
            bail!("no {} — wrong binary name? pass --bin", p.display());
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
