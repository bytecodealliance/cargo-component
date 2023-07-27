//! Core library of `cargo-component`.

#![deny(missing_docs)]

use anyhow::Context;
use semver::VersionReq;
use std::str::FromStr;
use warg_protocol::registry::PackageId;

pub mod keyring;
pub mod lock;
pub mod progress;
pub mod registry;
pub mod terminal;

/// Represents a versioned component package identifier.
#[derive(Clone)]
pub struct VersionedPackageId {
    /// The package identifier.
    pub id: PackageId,
    /// The optional package version.
    pub version: Option<VersionReq>,
}

impl FromStr for VersionedPackageId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.split_once('@') {
            Some((id, version)) => Ok(Self {
                id: id.parse()?,
                version: Some(
                    version
                        .parse()
                        .with_context(|| format!("invalid package version `{version}`"))?,
                ),
            }),
            None => Ok(Self {
                id: s.parse()?,
                version: None,
            }),
        }
    }
}
