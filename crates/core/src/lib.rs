//! Core library of `cargo-component`.

#![deny(missing_docs)]

use anyhow::Context;
use semver::VersionReq;
use std::str::FromStr;
use warg_protocol::registry::PackageName;

pub mod command;
pub mod lock;
pub mod progress;
pub mod registry;
pub mod terminal;

/// Represents a versioned component package name.
#[derive(Clone, Debug)]
pub struct VersionedPackageName {
    /// The package name.
    pub name: PackageName,
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

impl std::fmt::Display for VersionedPackageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(version) = &self.version {
            write!(f, "@{version}")?;
        }
        Ok(())
    }
}
