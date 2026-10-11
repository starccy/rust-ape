use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::host::link as core;

#[derive(Args)]
pub struct HostLinkArgs {
    /// x86_64 or aarch64
    #[arg(long)]
    arch: String,
    /// cosmocc's C compiler for that arch
    #[arg(long)]
    cc: PathBuf,
    /// and its ar, runnable as is
    #[arg(long)]
    ar: PathBuf,
    /// Where the archives go
    #[arg(long)]
    out_dir: PathBuf,
    /// Library search directories from the link line
    #[arg(short = 'L')]
    search: Vec<PathBuf>,
    /// Library names, as in -l<name>
    names: Vec<String>,
}

pub fn run(args: &HostLinkArgs) -> Result<()> {
    core::run(&core::Options {
        arch: args.arch.clone(),
        cc: args.cc.clone(),
        ar: args.ar.clone(),
        out_dir: args.out_dir.clone(),
        search: args.search.clone(),
        names: args.names.clone(),
    })
}
