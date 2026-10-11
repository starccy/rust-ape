use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::build as core;

#[derive(Args)]
pub struct BuildArgs {
    /// The project directory to build
    #[arg(value_name = "PROJECT_DIR")]
    project: PathBuf,
    #[arg(long)]
    release: bool,
    /// Link cosmocc's tiny runtime (-mtiny), which drops the built-in
    /// --strace/--ftrace machinery in exchange for a smaller binary
    #[arg(long)]
    tiny: bool,
    /// Force a std/libc rebuild (useful after hand-editing vendor/library)
    #[arg(long)]
    clean_std: bool,
    /// Build only this package of the project's workspace
    #[arg(long, short = 'p')]
    package: Option<String>,
    /// Build and pack only this binary
    #[arg(long)]
    bin: Option<String>,
    /// Build and pack only this example
    #[arg(long)]
    example: Option<String>,
    /// Space or comma separated list of features to activate
    #[arg(long, short = 'F')]
    features: Vec<String>,
    /// Activate all available features
    #[arg(long)]
    all_features: bool,
    /// Do not activate the `default` feature
    #[arg(long)]
    no_default_features: bool,
    /// Where to write the result (defaults to <project>/target/ape/<profile>/<name>.com)
    #[arg(long)]
    output: Option<PathBuf>,
}

pub fn run(args: &BuildArgs) -> Result<()> {
    core::run(&core::Options {
        project: args.project.clone(),
        release: args.release,
        tiny: args.tiny,
        clean_std: args.clean_std,
        package: args.package.clone(),
        bin: args.bin.clone(),
        example: args.example.clone(),
        features: args.features.clone(),
        all_features: args.all_features,
        no_default_features: args.no_default_features,
        output: args.output.clone(),
    })
}
