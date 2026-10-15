mod audit;
mod build;
mod cli;
mod host;
mod metadata;
mod scaffold;
mod test;
mod toolchain;
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
    /// Install everything: restore vendor/ (patched crates, cosmocc) and render generated/
    Setup(cli::setup::SetupArgs),
    /// Scaffold a minimal project wired up to the SDK
    Generate(cli::generate::GenerateArgs),
    /// Build both targets and apelink them into one fat APE
    Build(cli::build::BuildArgs),
    /// Build the test binaries as APEs and run them, like cargo test
    Test(cli::test::TestArgs),
    /// Append the files bundle.toml lists to an already built APE
    Bundle(cli::bundle::BundleArgs),
    /// Check libc crate constants and direct libc imports against cosmo
    Audit(cli::audit::AuditArgs),
    /// Turn bindgen output into #[ape::host_library] declarations
    HostBindings(cli::host_bindings::HostBindArgs),
    /// Make lazy-binding stubs for host shared libraries (run by the linker wrapper)
    #[command(hide = true)]
    Hostlink(cli::hostlink::HostLinkArgs),
    /// Build the TLS plugin for a release (run by .github/workflows/tls-plugin.yml)
    #[command(hide = true)]
    TlsPlugin(cli::tls_plugin::TlsPluginArgs),
}

fn main() -> Result<()> {
    // as RUSTC_WRAPPER, cargo runs us as `xtask <rustc> <args>`
    let wrapped = std::env::args_os()
        .nth(1)
        .is_some_and(|a| std::path::Path::new(&a).is_file());
    if std::env::var_os(host::rustcwrap::ENV).is_some() && wrapped {
        std::process::exit(host::rustcwrap::run(std::env::args_os().skip(1).collect()));
    }
    match Cli::parse().cmd {
        Cmd::Setup(args) => cli::setup::run(&args),
        Cmd::Generate(args) => cli::generate::run(&args),
        Cmd::Build(args) => cli::build::run(&args),
        Cmd::Test(args) => cli::test::run(&args),
        Cmd::Bundle(args) => cli::bundle::run(&args),
        Cmd::Audit(args) => cli::audit::run(&args),
        Cmd::HostBindings(args) => cli::host_bindings::run(&args),
        Cmd::Hostlink(args) => cli::hostlink::run(&args),
        Cmd::TlsPlugin(args) => cli::tls_plugin::run(&args),
    }
}
