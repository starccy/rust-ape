use anyhow::Result;
use clap::Args;

use crate::scaffold as core;

#[derive(Args)]
pub struct GenerateArgs {
    /// Directory for the new project (must not exist, or be empty)
    path: std::path::PathBuf,
    /// Package name (defaults to the directory name)
    #[arg(long)]
    name: Option<String>,
}

pub fn run(args: &GenerateArgs) -> Result<()> {
    core::run(&core::Options {
        path: args.path.clone(),
        name: args.name.clone(),
    })
}
