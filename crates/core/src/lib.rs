//! Core library of `cargo-component`.

#![deny(missing_docs)]

use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Context;
use semver::VersionReq;
use wasm_pkg_client::PackageRef;

pub mod command;
pub mod progress;
pub mod terminal;

/// The root directory name used for default cargo component directories
pub const CARGO_COMPONENT_DIR: &str = "cargo-component";
/// The cache directory name used by default
pub const CACHE_DIR: &str = "cache";

/// Returns the path to the default cache directory, returning an error if a cache directory cannot be found.
pub fn default_cache_dir() -> anyhow::Result<PathBuf> {
    dirs::cache_dir()
        .map(|p| p.join(CARGO_COMPONENT_DIR).join(CACHE_DIR))
        .ok_or_else(|| anyhow::anyhow!("failed to find cache directory"))
}

/// A helper that fetches the default directory if the given directory is `None`.
pub fn cache_dir(dir: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match dir {
        Some(dir) => Ok(dir),
        None => default_cache_dir(),
    }
}

/// Represents a versioned component package name.
#[derive(Clone)]
pub struct VersionedPackageName {
    /// The package name.
    pub name: PackageRef,
    /// The optional package version.
    pub version: Option<VersionReq>,
}

impl FromStr for VersionedPackageName {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.split_once('@') {
            Some((name, version)) => Ok(Self {
                name: name.parse()?,
                version: Some(
                    version
                        .parse()
                        .with_context(|| format!("invalid package version `{version}`"))?,
                ),
            }),
            None => Ok(Self {
                name: s.parse()?,
                version: None,
            }),
        }
    }
}
