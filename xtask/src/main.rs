mod audit;
mod build;
mod bundle;
mod concat;
mod config;
mod fetch;
mod generate;
mod genshim;
mod metadata;
mod setup;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Installs and drives the rust-ape toolchain")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install everything: restore vendor/ (rust-src, patched crates, cosmocc) and render generated/
    Setup(setup::SetupArgs),
    /// Scaffold a minimal project wired up to the SDK
    Generate(generate::GenerateArgs),
    /// Build both targets and apelink them into one fat APE
    Build(build::BuildArgs),
    /// Append the files bundle.toml lists to an already built APE
    Bundle(bundle::BundleArgs),
    /// Regenerate shim/tables.h and its compile-time cross-check from vendor/
    GenShim(genshim::GenShimArgs),
    /// Check libc crate constants and direct libc imports against cosmo
    Audit(audit::AuditArgs),
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Setup(args) => setup::run(&args),
        Cmd::Generate(args) => generate::run(&args),
        Cmd::Build(args) => build::run(&args),
        Cmd::Bundle(args) => bundle::run(&args),
        Cmd::GenShim(args) => genshim::run(&args),
        Cmd::Audit(args) => audit::run(&args),
    }
}
