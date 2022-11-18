//! Module for interacting with local and remote component registries.
use crate::{
    config::Config,
    metadata::{self, PackageId},
};
use anyhow::{bail, Context, Result};
use semver::{Version, VersionReq};
use std::{
    collections::{hash_map::Entry, HashMap},
    fs,
    path::PathBuf,
};
use url::Url;
use warg_crypto::hash::DynHash;
use wit_component::DecodedWasm;

pub mod local;

/// The name of the default registry.
pub const DEFAULT_REGISTRY_NAME: &str = "default";

/// Represents where content for a resolved package is located.
#[derive(Clone, Debug)]
pub enum PackageContentLocation {
    /// The content is already located on the local filesystem.
    Path(PathBuf),
    /// The content is located at the given URL.
    ///
    /// It is assumed that no authentication is required to access the URL.
    Url(Url),
}

/// Represents information about a package resolution.
#[derive(Clone, Debug)]
pub struct PackageResolution {
    /// The URL of the resolved package.
    pub url: Url,
    /// The package version that was resolved.
    pub version: Version,
    /// The digest of the package contents.
    pub digest: DynHash,
    /// The location of the package contents.
    pub location: PackageContentLocation,
}

/// A trait for interacting with component registries.
#[async_trait::async_trait]
pub trait Registry {
    /// Synchronizes the registry for the given package ids.
    ///
    /// For remote registries, this will update cached package logs
    /// via network access.
    async fn synchronize(&self, packages: &[&PackageId]) -> Result<()>;

    /// Resolves a package to the latest version that satisfies
    /// the given version requirement.
    ///
    /// If the version requirement is `None`, then the latest released
    /// version will be resolved.
    ///
    /// Returns `Ok(None)` if no version satisfies the requirement.
    ///
    /// Yanked packages will not be considered.
    fn resolve(
        &self,
        id: &PackageId,
        requirement: Option<&VersionReq>,
    ) -> Result<Option<PackageResolution>>;
}

/// Creates a registry implementation for the given registry metadata.
pub fn create(
    config: &Config,
    name: &str,
    registries: &HashMap<String, metadata::Registry>,
) -> Result<Box<dyn Registry>> {
    match registries.get(name) {
        Some(metadata::Registry::Remote(_)) => {
            // TODO: support remote registries
            bail!("remote registries are not yet supported")
        }
        Some(metadata::Registry::Local(path)) => {
            Ok(Box::new(local::LocalRegistry::open(config, path, true)?))
        }
        None if name != DEFAULT_REGISTRY_NAME => {
            bail!("component registry `{name}` does not exist")
        }
        None => {
            // TODO: support a default registry
            bail!("a default registry is not yet supported (it must be explicitly specified)");
        }
    }
}

/// Represents a resolved dependency.
#[derive(Debug, Clone)]
pub struct ResolvedDependency<'a> {
    /// The name of the dependency that was resolved.
    pub name: &'a str,
    /// The package id of the dependency.
    ///
    /// This is `None` for dependencies specified by path.
    pub id: Option<&'a PackageId>,
    /// The resolved URL for the dependency.
    pub url: Url,
    /// The version that was resolved.
    ///
    /// This is `None` for dependencies specified by path.
    pub version: Option<Version>,
    /// The digest of the dependency contents.
    ///
    /// This is `None` for dependencies specified by path.
    pub digest: Option<DynHash>,
    /// The path to the dependency contents.
    pub path: PathBuf,
}

impl ResolvedDependency<'_> {
    /// Gets the package name of the resolved dependency.
    pub fn package_name(&self) -> &str {
        match self.id {
            Some(id) => &id.name,
            None => self.name,
        }
    }

    /// Decodes the resolved dependency.
    pub fn decode(&self) -> Result<DecodedWasm> {
        let bytes = fs::read(&self.path).with_context(|| {
            format!(
                "failed to read content of dependency `{name}` at path `{path}`",
                name = self.name,
                path = self.path.display()
            )
        })?;

        let mut decoded =
            wit_component::decode(self.package_name(), &bytes).with_context(|| {
                format!(
                    "failed to decode content of dependency `{name}` at path `{path}`",
                    name = self.name,
                    path = self.path.display()
                )
            })?;

        // Set the URL of the package to the resolved URL.
        let package = decoded.package();
        let resolve = match &mut decoded {
            DecodedWasm::WitPackage(r, _) | DecodedWasm::Component(r, _) => r,
        };
        resolve.packages[package].url = Some(self.url.to_string());

        Ok(decoded)
    }
}

/// Represents a resolution of dependencies.
#[derive(Default, Debug, Clone)]
pub struct DependencyResolution<'a> {
    /// The resolved dependencies for the target world.
    pub target: HashMap<String, ResolvedDependency<'a>>,
    /// The resolved dependencies of the component itself.
    pub component: HashMap<String, ResolvedDependency<'a>>,
}

/// Resolves all component metadata dependencies.
///
/// `target` is the set of dependencies for the target world.
///
/// `component` is the set of dependencies the component itself depends on.
pub async fn resolve<'a>(
    config: &Config,
    registries: &HashMap<String, metadata::Registry>,
    target: &'a HashMap<String, metadata::Dependency>,
    component: &'a HashMap<String, metadata::Dependency>,
) -> Result<DependencyResolution<'a>> {
    struct PackageEntry<'a> {
        name: &'a str,
        package: &'a metadata::RegistryPackage,
        target: bool,
    }

    struct RegistryEntry<'a> {
        registry: Box<dyn Registry>,
        packages: HashMap<&'a PackageId, Vec<PackageEntry<'a>>>,
    }

    // First create a map of registries to package dependencies from that registry
    let mut resolution = DependencyResolution::default();
    let mut map = HashMap::new();
    for (name, dependency, target) in target
        .iter()
        .map(|(k, v)| (k, v, true))
        .chain(component.iter().map(|(k, v)| (k, v, false)))
    {
        match dependency {
            metadata::Dependency::Package(package) => {
                let entry = match map.entry(
                    package
                        .registry
                        .as_deref()
                        .unwrap_or(DEFAULT_REGISTRY_NAME)
                        .to_string(),
                ) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(e) => {
                        let registry = create(config, e.key(), registries)?;
                        e.insert(RegistryEntry {
                            registry,
                            packages: Default::default(),
                        })
                    }
                };

                let packages = entry.packages.entry(&package.id).or_default();
                packages.push(PackageEntry {
                    name,
                    package,
                    target,
                });
            }
            metadata::Dependency::Local(p) => {
                let resolutions = if target {
                    &mut resolution.target
                } else {
                    &mut resolution.component
                };

                let prev = resolutions.insert(
                    name.clone(),
                    ResolvedDependency {
                        name,
                        id: None,
                        url: Url::from_file_path(fs::canonicalize(p).with_context(|| {
                            format!(
                                "failed to canonicalize dependency path `{path}`",
                                path = p.display()
                            )
                        })?)
                        .unwrap(),
                        version: None,
                        digest: None,
                        path: p.clone(),
                    },
                );
                assert!(prev.is_none());
            }
        }
    }

    // Synchronize each registry first
    if config.cargo().network_allowed() && !map.is_empty() {
        config
            .cargo()
            .shell()
            .status("Updating", "component registry logs")?;

        for entry in map.values() {
            entry
                .registry
                .synchronize(&entry.packages.keys().copied().collect::<Vec<_>>())
                .await?;
        }
    }

    // Resolve every package dependency
    for entry in map.into_values() {
        let registry = entry.registry;
        for (id, packages) in entry.packages {
            for entry in packages {
                match registry.resolve(id, Some(&entry.package.version))? {
                    Some(r) => {
                        let path = match r.location {
                            PackageContentLocation::Path(p) => p,
                            PackageContentLocation::Url(_) => todo!("download package contents"),
                        };

                        let resolutions = if entry.target {
                            &mut resolution.target
                        } else {
                            &mut resolution.component
                        };

                        resolutions.insert(
                            entry.name.to_string(),
                            ResolvedDependency {
                                name: entry.name,
                                id: Some(id),
                                url: r.url,
                                version: Some(r.version),
                                digest: Some(r.digest),
                                path,
                            },
                        );
                    }
                    None => {
                        bail!("a version of package `{id}` that satisfies version requirement `{version}` was not found", version = entry.package.version);
                    }
                }
            }
        }
    }

    Ok(resolution)
}
