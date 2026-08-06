//! The slice of `cargo metadata` we actually read.

use crate::config;
use crate::util;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

#[derive(Deserialize)]
pub struct Metadata {
    pub packages: Vec<Package>,
    /// Package ids of the workspace's own members, as opposed to the full
    /// dependency graph in `packages`.
    pub workspace_members: Vec<String>,
    pub resolve: Resolve,
}

#[derive(Deserialize)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub version: String,
    /// None for path and git dependencies; a registry URL otherwise. This is
    /// how we tell a patched crate from one that slipped back to crates.io.
    pub source: Option<String>,
    pub targets: Vec<Target>,
}

#[derive(Deserialize)]
pub struct Target {
    pub name: String,
    /// "bin", "lib", "test"
    pub kind: Vec<String>,
}

#[derive(Deserialize)]
pub struct Resolve {
    /// None in a virtual workspace, where no single package is the root.
    pub root: Option<String>,
}

impl Package {
    /// Whether cargo resolved this one from the registry rather than a patch.
    pub fn from_registry(&self) -> bool {
        self.source
            .as_deref()
            .is_some_and(|s| s.contains("crates.io"))
    }
}

pub fn load(project: &Path) -> Result<Metadata> {
    let out = util::capture_with_stderr(
        Command::new("cargo")
            .current_dir(project)
            .env("RUSTUP_TOOLCHAIN", config::rust_channel()?)
            .args(["metadata", "--format-version", "1"]),
    )?;
    serde_json::from_str(&out).context("could not parse cargo metadata")
}
