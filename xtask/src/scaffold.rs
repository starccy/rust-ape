use crate::config::{self, PATCHED_CRATES};
use crate::util;
use anyhow::{Context, Result, bail};
use clap::Args;
use std::fs;

#[derive(Args)]
pub struct GenerateArgs {
    /// Directory for the new project (must not exist, or be empty)
    pub path: std::path::PathBuf,
    /// Package name (defaults to the directory name)
    #[arg(long)]
    pub name: Option<String>,
}

pub fn run(args: &GenerateArgs) -> Result<()> {
    let root = util::repo_root();
    if args.path.exists() && fs::read_dir(&args.path)?.next().is_some() {
        bail!("directory is not empty: {}", args.path.display());
    }
    fs::create_dir_all(args.path.join("src"))?;
    let dir = args.path.canonicalize()?;

    util::git_init(&dir)
        .inspect_err(|e| eprintln!("could not initialize git repo in {}: {e}", dir.display()))
        .ok();

    let name = match &args.name {
        Some(n) => n.clone(),
        None => dir
            .file_name()
            .context("could not take a package name from the path; use --name")?
            .to_string_lossy()
            .into_owned(),
    };
    if name.is_empty()
        || !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("not a valid package name: {name:?} (use --name)");
    }

    // All patch entries go in unconditionally: cargo ignores the ones nothing
    // depends on, and build verifies the rest actually took effect.
    let mut patches = String::new();
    for &(c, _) in PATCHED_CRATES {
        patches += &format!(
            "{c} = {{ path = \"{}\" }}\n",
            root.join("vendor/patches").join(c).display()
        );
    }
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
# What cosmo offers beyond libc: which host we actually landed on, cpu and
# memory info, and so on. Drop it if you don't need any of that.
ape = {{ path = "{ape}" }}

# rust-ape: these crates come from the SDK's cosmo-adapted copies. Entries
# nothing depends on cost nothing, so there's no need to prune them. If a
# newer crates.io release ever orphans one, `cargo xtask build` says so and
# tells you which `cargo update --precise` fixes it.
[patch.crates-io]
{patches}"#,
            ape = root.join("ape").display()
        ),
    )?;
    fs::write(
        dir.join("rust-toolchain.toml"),
        format!(
            "[toolchain]\nchannel = \"{}\"\ncomponents = [\"rust-src\"]\n",
            config::rust_channel()?
        ),
    )?;
    fs::write(
        dir.join("src/main.rs"),
        // Print the host: the same binary answers differently on Linux, Windows
        // and macOS, which is the whole point of building this way.
        "fn main() {\n    println!(\"hello from rust-ape, running on {:?}\", ape::current_os());\n}\n",
    )?;
    fs::write(dir.join(".gitignore"), "/target\n")?;

    println!("created {}", dir.display());
    println!(
        "build it with: (cd {} && cargo xtask build {})",
        root.display(),
        dir.display()
    );
    Ok(())
}
