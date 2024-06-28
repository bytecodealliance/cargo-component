//! Module for the lock file implementation.
use std::{collections::HashMap, path::Path};

use anyhow::Result;
use cargo_component_core::{
    lock::{FileLock, LockFile, LockedPackage, LockedPackageVersion},
    registry::{DependencyResolution, DependencyResolutionMap},
    terminal::{Colors, Terminal},
};
use semver::Version;
use wasm_pkg_client::{ContentDigest, PackageRef};

/// The name of the lock file.
pub const LOCK_FILE_NAME: &str = "wit.lock";

pub(crate) fn acquire_lock_file_ro(
    terminal: &Terminal,
    config_path: &Path,
) -> Result<Option<FileLock>> {
    let path = config_path.with_file_name(LOCK_FILE_NAME);
    if !path.exists() {
        return Ok(None);
    }

    log::info!("opening lock file `{path}`", path = path.display());
    match FileLock::try_open_ro(&path)? {
        Some(lock) => Ok(Some(lock)),
        None => {
            terminal.status_with_color(
                "Blocking",
                format!("on access to lock file `{path}`", path = path.display()),
                Colors::Cyan,
            )?;

            FileLock::open_ro(&path).map(Some)
        }
    }
}

pub(crate) fn acquire_lock_file_rw(terminal: &Terminal, config_path: &Path) -> Result<FileLock> {
    let path = config_path.with_file_name(LOCK_FILE_NAME);
    log::info!("creating lock file `{path}`", path = path.display());
    match FileLock::try_open_rw(&path)? {
        Some(lock) => Ok(lock),
        None => {
            terminal.status_with_color(
                "Blocking",
                format!("on access to lock file `{path}`", path = path.display()),
                Colors::Cyan,
            )?;

            FileLock::open_rw(&path)
        }
    }
}

/// Constructs a `LockFile` from a `DependencyResolutionMap`.
pub fn to_lock_file(map: &DependencyResolutionMap) -> LockFile {
    type PackageKey = (PackageRef, Option<String>);
    type VersionsMap = HashMap<String, (Version, ContentDigest)>;
    let mut packages: HashMap<PackageKey, VersionsMap> = HashMap::new();

    for resolution in map.values() {
        match resolution.key() {
            Some((id, registry)) => {
                let pkg = match resolution {
                    DependencyResolution::Registry(pkg) => pkg,
                    DependencyResolution::Local(_) => unreachable!(),
                };

                let prev = packages
                    .entry((id.clone(), registry.map(str::to_string)))
                    .or_default()
                    .insert(
                        pkg.requirement.to_string(),
                        (pkg.version.clone(), pkg.digest.clone()),
                    );

                if let Some((prev, _)) = prev {
                    // The same requirements should resolve to the same version
                    assert!(prev == pkg.version)
                }
            }
            None => continue,
        }
    }

    let mut packages: Vec<_> = packages
        .into_iter()
        .map(|((name, registry), versions)| {
            let mut versions: Vec<LockedPackageVersion> = versions
                .into_iter()
                .map(|(requirement, (version, digest))| LockedPackageVersion {
                    requirement,
                    version,
                    digest,
                })
                .collect();

            versions.sort_by(|a, b| a.key().cmp(b.key()));

            LockedPackage {
                name,
                registry,
                versions,
            }
        })
        .collect();

    packages.sort_by(|a, b| a.key().cmp(&b.key()));

    LockFile::new(packages)
}
