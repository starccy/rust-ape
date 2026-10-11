use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::test as core;

#[derive(Args)]
pub struct TestArgs {
    /// The project directory to test
    #[arg(value_name = "PROJECT_DIR")]
    project: PathBuf,
    #[arg(long)]
    release: bool,
    /// Link cosmocc's tiny runtime (-mtiny)
    #[arg(long)]
    tiny: bool,
    /// Force a std/libc rebuild
    #[arg(long)]
    clean_std: bool,
    /// Test only this package of the project's workspace
    #[arg(long, short = 'p')]
    package: Option<String>,
    /// Space or comma separated list of features to activate
    #[arg(long, short = 'F')]
    features: Vec<String>,
    /// Activate all available features
    #[arg(long)]
    all_features: bool,
    /// Do not activate the `default` feature
    #[arg(long)]
    no_default_features: bool,
    /// Test only the library's unit tests
    #[arg(long)]
    lib: bool,
    /// Test only the binaries' unit tests
    #[arg(long)]
    bins: bool,
    /// Test only the integration tests
    #[arg(long)]
    tests: bool,
    /// Test only this integration test
    #[arg(long, value_name = "NAME")]
    test: Vec<String>,
    /// Build and link the test binaries, but don't run them
    #[arg(long)]
    no_run: bool,
    /// Run every test binary even after one fails
    #[arg(long)]
    no_fail_fast: bool,
    /// Arguments for every test binary, such as a filter or --test-threads=1
    #[arg(last = true)]
    test_args: Vec<String>,
}

pub fn run(args: &TestArgs) -> Result<()> {
    let mut filters = Vec::new();
    for (on, flag) in [
        (args.lib, "--lib"),
        (args.bins, "--bins"),
        (args.tests, "--tests"),
    ] {
        if on {
            filters.push(flag.to_string());
        }
    }
    for t in &args.test {
        filters.extend(["--test".to_string(), t.clone()]);
    }
    core::run(&core::Options {
        project: args.project.clone(),
        release: args.release,
        tiny: args.tiny,
        clean_std: args.clean_std,
        package: args.package.clone(),
        features: args.features.clone(),
        all_features: args.all_features,
        no_default_features: args.no_default_features,
        filters,
        no_run: args.no_run,
        no_fail_fast: args.no_fail_fast,
        test_args: args.test_args.clone(),
    })
}
