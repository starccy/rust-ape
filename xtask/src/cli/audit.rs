use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::audit as core;

#[derive(Args)]
pub struct AuditArgs {
    /// cosmopolitan source tree, for libc/sysv/consts.sh
    /// (default: vendor/cosmocc, then the directory it links into)
    #[arg(long, env = "RUST_APE_COSMO_SRC")]
    cosmo_src: Option<PathBuf>,
    /// Project whose built crates are scanned for libc imports
    #[arg(long, default_value = "examples")]
    project: PathBuf,
    /// Cargo profile directory of that build
    #[arg(long, default_value = "debug")]
    profile: String,
    /// Sweep everything, not only what this build uses: constants nothing
    /// references, and every function the libc crate declares that goes
    /// straight to cosmo
    #[arg(long)]
    all: bool,
    /// Rewrite patches/std-imports.txt from this build instead of checking
    /// against it
    #[arg(long)]
    update_std_imports: bool,
}

pub fn run(args: &AuditArgs) -> Result<()> {
    core::run(&core::Options {
        cosmo_src: args.cosmo_src.clone(),
        project: args.project.clone(),
        profile: args.profile.clone(),
        all: args.all,
        update_std_imports: args.update_std_imports,
    })
}
