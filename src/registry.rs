//! Module for interacting with component registries.

use crate::{config::Config, metadata::ComponentMetadata};
use anyhow::Result;
use cargo_component_core::{
    lock::{LockFile, LockFileResolver, LockedPackage, LockedPackageVersion},
    registry::{DependencyResolution, DependencyResolutionMap, DependencyResolver},
};
use semver::Version;
use std::collections::HashMap;
use warg_crypto::hash::AnyHash;
use warg_protocol::registry::PackageId;

/// Represents a resolution of dependencies for a Cargo package.
#[derive(Debug, Clone)]
pub struct PackageDependencyResolution<'a> {
    /// The package component metadata.
    pub metadata: &'a ComponentMetadata,
    /// Resolutions for the package's target dependencies.
    pub target_resolutions: DependencyResolutionMap,
    /// Resolutions for the package's component dependencies.
    pub resolutions: DependencyResolutionMap,
}

impl<'a> PackageDependencyResolution<'a> {
    /// Creates a new package dependency resolution for the given package.
    ///
    /// Returns `Ok(None)` if the package is not a component package.
    pub async fn new(
        config: &Config,
        metadata: &'a ComponentMetadata,
        lock_file: Option<LockFileResolver<'_>>,
        network_allowed: bool,
    ) -> Result<PackageDependencyResolution<'a>> {
        Ok(Self {
            metadata,
            target_resolutions: Self::resolve_target_deps(
                config,
                metadata,
                lock_file,
                network_allowed,
            )
            .await?,
            resolutions: Self::resolve_deps(config, metadata, lock_file, network_allowed).await?,
        })
    }

    /// Iterates over all dependency resolutions of the package.
    pub fn all(&self) -> impl Iterator<Item = (&PackageId, &DependencyResolution)> {
        self.target_resolutions
            .iter()
            .chain(self.resolutions.iter())
    }

    async fn resolve_target_deps(
        config: &Config,
        metadata: &ComponentMetadata,
        lock_file: Option<LockFileResolver<'_>>,
        network_allowed: bool,
    ) -> Result<DependencyResolutionMap> {
        let target_deps = metadata
            .section
            .target
            .as_ref()
            .map(|t| t.dependencies())
            .unwrap_or_default();

        let mut resolver = DependencyResolver::new(
            config.warg(),
            &metadata.section.registries,
            lock_file,
            config.terminal(),
            network_allowed,
        )?;

        for (name, dependency) in target_deps.iter() {
            resolver.add_dependency(name, dependency).await?;
        }

        resolver.resolve().await
    }

    async fn resolve_deps(
        config: &Config,
        metadata: &ComponentMetadata,
        lock_file: Option<LockFileResolver<'_>>,
        network_allowed: bool,
    ) -> Result<DependencyResolutionMap> {
        let mut resolver = DependencyResolver::new(
            config.warg(),
            &metadata.section.registries,
            lock_file,
            config.terminal(),
            network_allowed,
        )?;

        for (name, dependency) in &metadata.section.dependencies {
            resolver.add_dependency(name, dependency).await?;
        }

        resolver.resolve().await
    }
}

/// Represents a mapping between all component packages and their dependency resolutions.
#[derive(Debug, Default, Clone)]
pub struct PackageResolutionMap<'a>(
    HashMap<cargo_metadata::PackageId, PackageDependencyResolution<'a>>,
);

impl<'a> PackageResolutionMap<'a> {
    /// Inserts a package dependency resolution into the map.
    ///
    /// # Panics
    ///
    /// Panics if the package already has a dependency resolution.
    pub fn insert(
        &mut self,
        id: cargo_metadata::PackageId,
        resolution: PackageDependencyResolution<'a>,
    ) {
        let prev = self.0.insert(id, resolution);
        assert!(prev.is_none());
    }

    /// Gets a package dependency resolution from the map.
    ///
    /// Returns `None` if the package has no dependency resolution.
    pub fn get(&self, id: &cargo_metadata::PackageId) -> Option<&PackageDependencyResolution<'a>> {
        self.0.get(id)
    }

    /// Converts the resolution map into a lock file.
    pub fn to_lock_file(&self) -> LockFile {
        type PackageKey = (PackageId, Option<String>);
        type VersionsMap = HashMap<String, (Version, AnyHash)>;
        let mut packages: HashMap<PackageKey, VersionsMap> = HashMap::new();

        for resolution in self.0.values() {
            for (_, dep) in resolution.all() {
                match dep.key() {
                    Some((id, registry)) => {
                        let pkg = match dep {
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
        }

        let mut packages: Vec<_> = packages
            .into_iter()
            .map(|((id, registry), versions)| {
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
                    id,
                    registry,
                    versions,
                }
            })
            .collect();

        packages.sort_by(|a, b| a.key().cmp(&b.key()));

        LockFile::new(packages)
    }
}
