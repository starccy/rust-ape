use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::toolchain::tls_plugin;
use crate::util;

#[derive(Args)]
pub struct TlsPluginArgs {
    /// Directory to put the plugin in, under its release asset name
    #[arg(long, value_name = "DIR")]
    out: PathBuf,
}

/// Builds the plugin for a release; prints `tag=<release tag>` for the workflow
pub fn run(args: &TlsPluginArgs) -> Result<()> {
    let root = util::repo_root();
    tls_plugin::fetch(&root, true, true)?;
    let (tag, asset) = tls_plugin::release_names()?;
    fs::create_dir_all(&args.out)?;
    fs::copy(tls_plugin::path(&root), args.out.join(&asset))?;
    println!("tag={tag}");
    Ok(())
}
