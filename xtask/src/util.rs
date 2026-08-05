use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Repo root, i.e. the directory above xtask/.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must sit one level below the repo root")
        .to_path_buf()
}

pub fn run(cmd: &mut Command) -> Result<()> {
    let desc = format!("{cmd:?}");
    let status = cmd
        .status()
        .with_context(|| format!("could not run: {desc}"))?;
    if !status.success() {
        bail!("failed ({status}): {desc}");
    }
    Ok(())
}

pub fn capture(cmd: &mut Command) -> Result<String> {
    let desc = format!("{cmd:?}");
    let out = cmd
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("could not run: {desc}"))?;
    if !out.status.success() {
        bail!("failed ({}): {desc}", out.status);
    }
    Ok(String::from_utf8(out.stdout)?)
}

/// APE binaries start with MZ and exec'ing one gives ENOEXEC unless binfmt_misc
/// is registered, so run those through sh script
pub fn ape_command(program: &Path) -> Command {
    let is_ape = {
        use std::io::Read;
        let mut magic = [0u8; 2];
        fs::File::open(program)
            .and_then(|mut f| f.read_exact(&mut magic))
            .map(|()| &magic == b"MZ")
            .unwrap_or(false)
    };
    if is_ape {
        let mut c = Command::new("sh");
        c.arg(program);
        c
    } else {
        Command::new(program)
    }
}

/// Like capture, but leaves stderr on the terminal where you can read it.
pub fn capture_with_stderr(cmd: &mut Command) -> Result<String> {
    let desc = format!("{cmd:?}");
    let out = cmd
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("could not run: {desc}"))?;
    if !out.status.success() {
        bail!("failed ({}): {desc}", out.status);
    }
    Ok(String::from_utf8(out.stdout)?)
}

/// Downloads url to dest, treating an existing dest as a cache hit. Writes to
/// .part first so an interrupted run can't leave something that looks cached.
pub fn download(url: &str, dest: &Path) -> Result<()> {
    if dest.exists() {
        println!("==> cached: {}", dest.display());
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let part = dest.with_file_name(format!(
        "{}.part",
        dest.file_name().unwrap().to_string_lossy()
    ));
    println!("==> downloading {url}");
    run(Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-A", "rust-ape-xtask", "-o"])
        .arg(&part)
        .arg(url))?;
    fs::rename(&part, dest)?;
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f =
        fs::File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    // sha2 0.11 returns a hybrid_array::Array, which has no LowerHex impl.
    Ok(hasher.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    }))
}

pub fn apply_patch(dir: &Path, patch_file: &Path) -> Result<()> {
    println!(
        "==> applying {}",
        patch_file.file_name().unwrap().to_string_lossy()
    );
    let f = fs::File::open(patch_file)
        .with_context(|| format!("could not open {}", patch_file.display()))?;
    run(Command::new("patch")
        .args(["-p1", "-s"])
        .arg("-d")
        .arg(dir)
        .stdin(f))
}

pub fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    run(Command::new("cp").arg("-a").arg(src).arg(dst))
}

pub fn rm_rf(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("could not remove {}", path.display())),
    }
}

pub fn up_to_date(dir: &Path, stamp_file: &Path, expected: &str) -> bool {
    dir.is_dir()
        && fs::read_to_string(stamp_file)
            .map(|s| s.trim() == expected)
            .unwrap_or(false)
}

pub fn write_stamp(stamp_file: &Path, value: &str) -> Result<()> {
    if let Some(parent) = stamp_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(stamp_file, format!("{value}\n"))?;
    Ok(())
}

pub fn find_dir_suffix(base: &Path, suffix: &str) -> Option<PathBuf> {
    if base.ends_with(suffix) {
        return Some(base.to_path_buf());
    }
    for entry in fs::read_dir(base).ok()? {
        let p = entry.ok()?.path();
        if p.is_dir()
            && let Some(found) = find_dir_suffix(&p, suffix)
        {
            return Some(found);
        }
    }
    None
}

pub fn git_init(dir: &Path) -> Result<()> {
    run(Command::new("git")
        .arg("init")
        .arg(dir)
    )
}