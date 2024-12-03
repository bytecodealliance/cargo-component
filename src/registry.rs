//! Module for interacting with component registries.
use std::collections::HashMap;

use anyhow::Result;
use cargo_metadata::PackageId;
use semver::{Version, VersionReq};
use wasm_pkg_client::{
    caching::{CachingClient, FileCache},
    ContentDigest, PackageRef,
};
use wasm_pkg_core::{
    lock::{LockFile, LockedPackage, LockedPackageVersion},
    resolver::{Dependency, DependencyResolution, DependencyResolutionMap, DependencyResolver},
};

use crate::metadata::ComponentMetadata;

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
        client: CachingClient<FileCache>,
        metadata: &'a ComponentMetadata,
        lock_file: &LockFile,
    ) -> Result<PackageDependencyResolution<'a>> {
        Ok(Self {
            metadata,
            target_resolutions: Self::resolve_target_deps(client.clone(), metadata, lock_file)
                .await?,
            resolutions: Self::resolve_deps(client, metadata, lock_file).await?,
        })
    }

    /// Iterates over all dependency resolutions of the package.
    pub fn all(&self) -> impl Iterator<Item = (&PackageRef, &DependencyResolution)> {
        self.target_resolutions
            .iter()
            .chain(self.resolutions.iter())
    }

    async fn resolve_target_deps(
        client: CachingClient<FileCache>,
        metadata: &ComponentMetadata,
        lock_file: &LockFile,
    ) -> Result<DependencyResolutionMap> {
        let target_deps = metadata.section.target.dependencies();
        if target_deps.is_empty() {
            return Ok(Default::default());
        }

        let mut resolver = DependencyResolver::new_with_client(client, Some(lock_file))?;

        for (name, dependency) in target_deps.iter() {
            resolver.add_shallow_dependency(name, &dependency.0).await?;
        }

        resolver.resolve().await
    }

    async fn resolve_deps(
        client: CachingClient<FileCache>,
        metadata: &ComponentMetadata,
        lock_file: &LockFile,
    ) -> Result<DependencyResolutionMap> {
        if metadata.section.dependencies.is_empty() {
            return Ok(Default::default());
        }

        let mut resolver = DependencyResolver::new_with_client(client, Some(lock_file))?;

        for (name, dependency) in &metadata.section.dependencies {
            if let Dependency::Local(path) = dependency.clone().0 {
                resolver.add_shallow_dependency(name, &Dependency::Local(path)).await?;
                
            } else {
                resolver.add_dependency(name, &dependency.0).await?;
            }
        }

        resolver.resolve().await
    }
}

/// Represents a mapping between all component packages and their dependency resolutions.
#[derive(Debug, Default, Clone)]
pub struct PackageResolutionMap<'a>(HashMap<PackageId, PackageDependencyResolution<'a>>);

impl<'a> PackageResolutionMap<'a> {
    /// Inserts a package dependency resolution into the map.
    ///
    /// # Panics
    ///
    /// Panics if the package already has a dependency resolution.
    pub fn insert(&mut self, id: PackageId, resolution: PackageDependencyResolution<'a>) {
        let prev = self.0.insert(id, resolution);
        assert!(prev.is_none());
    }

    /// Gets a package dependency resolution from the map.
    ///
    /// Returns `None` if the package has no dependency resolution.
    pub fn get(&self, id: &PackageId) -> Option<&PackageDependencyResolution<'a>> {
        self.0.get(id)
    }

    /// Converts the resolution map into a lock file.
    pub async fn to_lock_file(&self) -> LockFile {
        type PackageKey = (PackageRef, Option<String>);
        type VersionsMap = HashMap<String, (Version, ContentDigest)>;
        let mut packages: HashMap<PackageKey, VersionsMap> = HashMap::new();

        for resolution in self.0.values() {
            for (_, dep) in resolution.all() {
                match dep.key() {
                    Some((name, registry)) => {
                        let pkg = match dep {
                            DependencyResolution::Registry(pkg) => pkg,
                            DependencyResolution::Local(_) => unreachable!(),
                        };

                        let prev = packages
                            .entry((name.clone(), registry.map(str::to_string)))
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
            .map(|((name, registry), versions)| {
                let mut versions: Vec<LockedPackageVersion> = versions
                    .into_iter()
                    .map(|(requirement, (version, digest))| LockedPackageVersion {
                        requirement: VersionReq::parse(&requirement).unwrap(),
                        version,
                        digest,
                    })
                    .collect();

                versions.sort_by(|a, b| a.key().cmp(&b.key()));

                LockedPackage {
                    name,
                    registry,
                    versions,
                }
            })
            .collect();

        packages.sort_by(|a, b| a.key().cmp(&b.key()));

        LockFile::new_with_path(packages, "wkg.lock").await.unwrap()
    }
}
