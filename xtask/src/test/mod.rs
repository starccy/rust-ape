//! `cargo xtask test`: builds a project's test binaries for both targets,
//! apelinks each pair into one APE under <target dir>/ape/<profile>/tests
//! and runs them here, one after another, the way `cargo test` does.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::build::{Session, TARGETS, apelink, selection_args};
use crate::util;

/// What the command was asked to do; cli/test.rs has the flags.
pub struct Options {
    pub project: PathBuf,
    pub release: bool,
    pub tiny: bool,
    pub clean_std: bool,
    pub package: Option<String>,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    /// --lib, --bins, --tests, --test NAME and so on, passed to cargo as is
    pub filters: Vec<String>,
    pub no_run: bool,
    pub no_fail_fast: bool,
    /// After `--`, for every test binary
    pub test_args: Vec<String>,
}

/// One test binary, linked
struct TestBin {
    /// What it was built from, as cargo prints it
    src: PathBuf,
    path: PathBuf,
    /// The package's directory, where cargo would run it
    package_dir: PathBuf,
}

pub fn run(args: &Options) -> Result<()> {
    let session = Session::open(&args.project, args.release, args.clean_std)?;
    let bins = build(&session, args)?;
    session.finish()?;
    if args.no_run {
        for bin in &bins {
            println!("  Executable {}", bin.path.display());
        }
        return Ok(());
    }

    let mut failed = Vec::new();
    for bin in &bins {
        let src = bin.src.strip_prefix(&bin.package_dir).unwrap_or(&bin.src);
        println!("     Running {} ({})", src.display(), bin.path.display());
        let status = util::ape_command(&bin.path)
            .current_dir(&bin.package_dir)
            .env("CARGO_MANIFEST_DIR", &bin.package_dir)
            .args(&args.test_args)
            .status()
            .with_context(|| format!("could not run {}", bin.path.display()))?;
        if !status.success() {
            failed.push(format!("{} ({status})", src.display()));
            if !args.no_fail_fast {
                break;
            }
        }
    }
    if !failed.is_empty() {
        bail!("test failed: {}", failed.join(", "));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Message {
    reason: String,
    #[serde(default)]
    package_id: String,
    #[serde(default)]
    manifest_path: PathBuf,
    target: Option<ArtifactTarget>,
    profile: Option<ArtifactProfile>,
    executable: Option<PathBuf>,
}

#[derive(Deserialize)]
struct ArtifactTarget {
    name: String,
    kind: Vec<String>,
    src_path: PathBuf,
}

#[derive(Deserialize)]
struct ArtifactProfile {
    test: bool,
}

fn build(session: &Session, args: &Options) -> Result<Vec<TestBin>> {
    // (package id, kind, target name) to its source, package directory
    // and the executable for each target
    type Found = (PathBuf, PathBuf, Vec<PathBuf>);
    let mut found: BTreeMap<(String, String, String), Found> = BTreeMap::new();
    for &(triple, arch) in TARGETS {
        println!("==> building tests for {triple} ({})", session.profile);
        let mut cmd = session.cargo(triple, arch, "test", args.tiny)?;
        cmd.args(["--no-run", "--message-format=json-render-diagnostics"]);
        cmd.args(selection_args(
            args.package.as_deref(),
            &args.features,
            args.all_features,
            args.no_default_features,
        ));
        cmd.args(&args.filters);
        let out = util::capture_with_stderr(&mut cmd)?;
        for line in out.lines() {
            let Ok(msg) = serde_json::from_str::<Message>(line) else {
                continue;
            };
            if msg.reason != "compiler-artifact" || !msg.profile.is_some_and(|p| p.test) {
                continue;
            }
            let (Some(target), Some(exe)) = (msg.target, msg.executable) else {
                continue;
            };
            let kind = target.kind.first().cloned().unwrap_or_default();
            let dir = msg
                .manifest_path
                .parent()
                .map(PathBuf::from)
                .unwrap_or_default();
            found
                .entry((msg.package_id, kind, target.name))
                .or_insert_with(|| (target.src_path, dir, Vec::new()))
                .2
                .push(exe);
        }
    }

    let out_dir = session
        .target_dir
        .join("ape")
        .join(session.profile)
        .join("tests");
    let mut bins = Vec::new();
    for ((package_id, kind, target), (src, package_dir, exes)) in found {
        if exes.len() != TARGETS.len() {
            bail!("{target} ({kind}) was built for only some of the targets: {exes:?}");
        }
        let package = session
            .meta
            .packages
            .iter()
            .find(|p| p.id == package_id)
            .map_or(target.as_str(), |p| p.name.as_str());
        let name = match kind.as_str() {
            "test" => target.clone(),
            "lib" | "proc-macro" if package.replace('-', "_") == target => {
                format!("{package}-unit")
            }
            _ => format!("{target}-{kind}"),
        };
        let path = out_dir.join(format!("{name}.com"));
        apelink(&session.root, &exes, &path, args.tiny)
            .with_context(|| format!("linking test binary {name}"))?;
        bins.push(TestBin {
            src,
            path,
            package_dir,
        });
    }
    Ok(bins)
}
