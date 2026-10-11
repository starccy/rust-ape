use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::build::bundle as core;

#[derive(Args)]
pub struct BundleArgs {
    /// The APE binary to append the bundle to
    #[arg(value_name = "BINARY")]
    binary: PathBuf,
    /// Project directory holding bundle.toml (default: the current directory)
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,
    /// Binary name to match `bins` against (default: the file name minus .com)
    #[arg(long, value_name = "NAME")]
    bin: Option<String>,
}

pub fn run(args: &BundleArgs) -> Result<()> {
    core::run(&core::Options {
        binary: args.binary.clone(),
        project: args.project.clone(),
        bin: args.bin.clone(),
    })
}
