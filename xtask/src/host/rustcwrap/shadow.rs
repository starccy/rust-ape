//! Compiling the crate from a copy of its tree in which the changed files
//! are written out and everything else is a symlink.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use super::crate_info::Crate;
use super::{command, exec};

// Compiles the crate from a copy in which `changed` files have the new
// text, every other file a symlink. If that fails, compiles it as it is.
pub(super) fn compile_copy(
    args: &[String],
    krate: &Crate,
    manifest: Option<&Path>,
    out: Option<&Path>,
    changed: HashMap<PathBuf, String>,
) -> Result<Option<i32>> {
    let base = krate
        .out_dir
        .parent()
        .unwrap_or(&krate.out_dir)
        .join("ape-hostlink")
        .join(format!("{}{}", krate.name, krate.extra));
    let _ = fs::remove_dir_all(&base);
    let mut roots: Vec<(PathBuf, PathBuf, &str)> = Vec::new();
    if let Some(m) = manifest {
        roots.push((m.to_path_buf(), base.join("m"), "CARGO_MANIFEST_DIR"));
    }
    if let Some(o) = out {
        roots.push((o.to_path_buf(), base.join("o"), "OUT_DIR"));
    }
    let mut env: Vec<(&str, PathBuf)> = Vec::new();
    for (orig, shadow, var) in &roots {
        let mine: HashMap<&PathBuf, &String> = changed
            .iter()
            .filter(|(p, _)| p.starts_with(orig))
            .collect();
        if mine.is_empty() {
            continue;
        }
        mirror(orig, shadow, &mine)?;
        env.push((var, shadow.clone()));
    }
    let Some(root_shadow) = roots
        .iter()
        .find(|(o, _, _)| krate.root.starts_with(o))
        .map(|(o, s, _)| s.join(krate.root.strip_prefix(o).unwrap()))
    else {
        bail!("{} isn't under the crate's directory", krate.root.display());
    };
    for p in changed.keys() {
        if !roots.iter().any(|(o, _, _)| p.starts_with(o)) {
            bail!(
                "{} is outside the crate's directory and OUT_DIR",
                p.display()
            );
        }
    }

    let mut new_args: Vec<String> = args
        .iter()
        .map(|a| {
            if *a == krate.root_arg {
                root_shadow.display().to_string()
            } else {
                a.clone()
            }
        })
        .collect();
    for (orig, shadow, _) in &roots {
        new_args.push(format!(
            "--remap-path-prefix={}={}",
            shadow.display(),
            orig.display()
        ));
    }
    let env_ref: Vec<(&str, &Path)> = env.iter().map(|(k, v)| (*k, v.as_path())).collect();
    let output = command(&new_args, &env_ref)
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("running {}", args[0]))?;
    if output.status.success() {
        fix_dep_info(krate, &roots)?;
        use std::io::Write;
        std::io::stderr().write_all(&output.stderr)?;
        return Ok(Some(0));
    }
    let log = base.join("failed.log");
    fs::write(&log, &output.stderr)?;
    eprintln!(
        "warning: rust-ape: {} didn't compile with host library wrappers, see {}; compiling it as it is",
        krate.name,
        log.display()
    );
    Ok(Some(exec(args, &[])))
}

// the files that changed get written, the rest of the tree is symlinked
fn mirror(orig: &Path, shadow: &Path, changed: &HashMap<&PathBuf, &String>) -> Result<()> {
    let mut dirs: HashSet<PathBuf> = HashSet::new();
    for p in changed.keys() {
        let mut d = p.parent();
        while let Some(x) = d {
            if !x.starts_with(orig) {
                break;
            }
            dirs.insert(x.to_path_buf());
            d = x.parent();
        }
    }
    for d in &dirs {
        let sd = shadow.join(d.strip_prefix(orig)?);
        fs::create_dir_all(&sd)?;
        for e in fs::read_dir(d)? {
            let e = e?;
            let p = e.path();
            if dirs.contains(&p) {
                continue;
            }
            let sp = sd.join(e.file_name());
            if let Some(t) = changed.get(&p) {
                fs::write(&sp, t)?;
            } else {
                std::os::unix::fs::symlink(&p, &sp)
                    .with_context(|| format!("linking {}", sp.display()))?;
            }
        }
    }
    Ok(())
}

fn fix_dep_info(krate: &Crate, roots: &[(PathBuf, PathBuf, &str)]) -> Result<()> {
    let d = krate
        .out_dir
        .join(format!("{}{}.d", krate.name, krate.extra));
    let Ok(mut text) = fs::read_to_string(&d) else {
        return Ok(());
    };
    for (orig, shadow, _) in roots {
        text = text.replace(&shadow.display().to_string(), &orig.display().to_string());
    }
    fs::write(&d, text)?;
    Ok(())
}
