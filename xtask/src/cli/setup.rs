use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::toolchain::setup as core;

#[derive(Args)]
pub struct SetupArgs {
    /// Ignore stamps and rebuild everything under vendor/
    #[arg(long)]
    force: bool,
    /// Use a locally built toolchain instead of the pinned release zip
    #[arg(long, env = "RUST_APE_COSMOCC", value_name = "DIR")]
    cosmocc: Option<PathBuf>,
    /// Build the TLS plugin here (needs a C++ compiler) instead of downloading it
    #[arg(long)]
    build_tls_plugin: bool,
}

pub fn run(args: &SetupArgs) -> Result<()> {
    core::run(&core::Options {
        force: args.force,
        cosmocc: args.cosmocc.clone(),
        build_tls_plugin: args.build_tls_plugin,
    })
}
