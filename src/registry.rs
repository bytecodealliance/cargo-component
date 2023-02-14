//! Module for interacting with local and remote component registries.
use crate::{
    config::Config,
    metadata::{self, ComponentMetadata, PackageId},
};
use anyhow::{bail, Context, Result};
use cargo::{
    core::{Package, Workspace},
    util::Filesystem,
};
use semver::{Comparator, Op, Version, VersionReq};
use serde::{de::IntoDeserializer, Deserialize, Serialize};
use std::{
    collections::{hash_map::Entry, HashMap},
    fs,
    io::{Read, Write},
    path::PathBuf,
};
use toml_edit::{Document, Item, Value};
use url::Url;
use warg_crypto::hash::DynHash;
use wit_component::DecodedWasm;

pub mod local;

/// The name of the default registry.
pub const DEFAULT_REGISTRY_NAME: &str = "default";
/// The name of the lock file used by cargo-component.
pub const LOCK_FILE_NAME: &str = "Cargo-component.lock";
/// The file format version of the lock file used by cargo-component.
pub const LOCK_FILE_VERSION: i64 = 1;

fn check_lock_update_allowed(config: &Config, workspace: &Workspace<'_>) -> Result<()> {
    if !config.cargo().lock_update_allowed() {
        let flag = if config.cargo().locked() {
            "--locked"
        } else {
            "--frozen"
        };
        anyhow::bail!(
            "the lock file {path} needs to be updated but {flag} was passed to prevent this\n\
            If you want to try to generate the lock file without accessing the network, \
            remove the {flag} flag and use --offline instead.",
            path = workspace
                .root()
                .to_path_buf()
                .join(LOCK_FILE_NAME)
                .display()
        );
    }

    Ok(())
}

/// Represents where content for a resolved package is located.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentLocation {
    /// The content is local.
    Local(PathBuf),
    /// The content is remote.
    Remote(Url),
}

/// Represents information about a resolution of a registry package.
#[derive(Clone, Debug)]
pub struct RegistryPackageResolution {
    /// The id of the package that was resolved.
    pub id: PackageId,
    /// The version requirement that was used to resolve the package.
    pub requirement: VersionReq,
    /// The URL of the resolved package.
    pub url: Url,
    /// The package version that was resolved.
    pub version: Version,
    /// The digest of the package contents.
    pub digest: DynHash,
    /// The location of the package contents.
    pub location: ContentLocation,
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
        requirement: &VersionReq,
    ) -> Result<Option<RegistryPackageResolution>>;
}

/// Creates a registry implementation for the given registry metadata.
pub fn create(
    config: &Config,
    name: Option<&str>,
    registries: &HashMap<String, metadata::Registry>,
) -> Result<Box<dyn Registry>> {
    let name = name.unwrap_or(DEFAULT_REGISTRY_NAME);
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

/// Represents version information for a locked package.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockedPackageVersion {
    /// The version requirement used to resolve this version.
    pub requirement: String,
    /// The version the package is locked to.
    pub version: Version,
    /// The digest of the package contents.
    pub digest: DynHash,
}

impl LockedPackageVersion {
    fn key(&self) -> &str {
        &self.requirement
    }
}

/// Represents a locked package in a lock file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct LockedPackage {
    /// The package identifier for the locked package.
    pub id: PackageId,
    /// The registry the package was resolved from.
    ///
    /// Defaults to the default registry if not specified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// The locked versions of a package.
    ///
    /// A package may have multiple locked versions if more than one
    /// version requirement was specified for the package in `Cargo.toml`.
    #[serde(rename = "version", default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<LockedPackageVersion>,
}

impl LockedPackage {
    /// The key used in sorting and searching the package list.
    fn key(&self) -> (&PackageId, &str) {
        (
            &self.id,
            self.registry.as_deref().unwrap_or(DEFAULT_REGISTRY_NAME),
        )
    }
}

/// Represents a resolved dependency lock file.
///
/// This is a TOML file that contains the resolved dependency information from
/// a previous build.
///
/// It functions like `Cargo.lock` in that it is used to ensure that the same
/// dependency is used between builds.
///
/// However, it deals with component registry dependencies instead of crate
/// dependencies.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct LockFile {
    /// The version of the lock file.
    ///
    /// Currently this is always `1`.
    pub version: i64,
    /// The locked dependencies in the lock file.
    ///
    /// This list is sorted by the key of the locked package.
    #[serde(rename = "package", default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<LockedPackage>,
}

impl LockFile {
    /// Constructs a `LockFile` from a `PackageResolutionMap`.
    pub fn from_resolution(map: &PackageResolutionMap) -> Self {
        type PackageKey = (PackageId, Option<String>);
        type VersionsMap = HashMap<String, (Version, DynHash)>;
        let mut packages: HashMap<PackageKey, VersionsMap> = HashMap::new();

        for resolution in map.values() {
            for (_, dep) in resolution.deps() {
                match dep.key() {
                    Some((id, registry)) => {
                        let pkg = dep.package.as_ref().unwrap();
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

        Self {
            version: LOCK_FILE_VERSION,
            packages,
        }
    }

    /// Opens the lock file for the given workspace.
    pub fn open(config: &Config, workspace: &Workspace) -> Result<Option<Self>> {
        let path = workspace.root().join(LOCK_FILE_NAME);

        if !path.exists() {
            return Ok(None);
        }

        let mut file = Filesystem::new(workspace.root().into())
            .open_ro(
                LOCK_FILE_NAME,
                config.cargo(),
                &format!("{LOCK_FILE_NAME} file"),
            )
            .with_context(|| format!("failed to open `{path}`", path = path.display()))?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .with_context(|| format!("failed to read `{path}`", path = path.display()))?;

        let document: Document = contents
            .parse()
            .with_context(|| format!("failed to parse `{path}`", path = path.display()))?;

        match document.as_table().get("version") {
            Some(Item::Value(Value::Integer(v))) => {
                if *v.value() != LOCK_FILE_VERSION {
                    bail!(
                        "failed to parse `{path}`: unsupported file format version {version}",
                        path = path.display(),
                        version = v.value()
                    );
                }

                // In the future, we should convert between supported versions here.
            }
            Some(_) => bail!(
                "failed to parse `{path}`: file format version is not an integer",
                path = path.display()
            ),
            None => bail!(
                "failed to parse `{path}`: missing file format version",
                path = path.display()
            ),
        }

        Ok(Some(
            Self::deserialize(document.into_deserializer()).with_context(|| {
                format!(
                    "failed to parse `{path}`: invalid file format",
                    path = path.display()
                )
            })?,
        ))
    }

    /// Resolves a package from the lock file.
    ///
    /// Returns `Ok(None)` if the package cannot be resolved.
    ///
    /// Fails if the package cannot be resolved and the lock file is not allowed to be updated.
    pub fn resolve(
        &self,
        config: &Config,
        workspace: &Workspace,
        registry: &str,
        id: &PackageId,
        requirement: &VersionReq,
    ) -> Result<Option<&LockedPackageVersion>> {
        if let Some(pkg) = self
            .packages
            .binary_search_by_key(&(id, registry), LockedPackage::key)
            .ok()
            .map(|i| &self.packages[i])
        {
            if let Ok(index) = pkg
                .versions
                .binary_search_by_key(&requirement.to_string().as_str(), LockedPackageVersion::key)
            {
                return Ok(Some(&pkg.versions[index]));
            }
        }

        check_lock_update_allowed(config, workspace)?;
        Ok(None)
    }

    /// Updates the lock file on disk given the new package resolution map.
    pub fn update(
        self,
        config: &Config,
        workspace: &Workspace<'_>,
        map: &PackageResolutionMap,
    ) -> Result<()> {
        let updated = Self::from_resolution(map);

        // If the set of packages are the same, we don't need to update the lock file.
        let path = workspace.root().join(LOCK_FILE_NAME);
        if path.is_file() && updated == self {
            return Ok(());
        }

        check_lock_update_allowed(config, workspace)?;

        let updated = toml_edit::ser::to_string_pretty(&updated)
            .with_context(|| format!("failed to serialize `{path}`", path = path.display()))?;

        let fs = Filesystem::new(workspace.root().into());
        let mut lock = fs
            .open_rw(
                LOCK_FILE_NAME,
                config.cargo(),
                &format!("{LOCK_FILE_NAME} file"),
            )
            .with_context(|| format!("failed to open `{path}`", path = path.display()))?;

        lock.file().set_len(0)?;
        lock.write_all(b"# This file is automatically generated by cargo-component.\n# It is not intended for manual editing.\n")
            .and_then(|_| lock.write_all(updated.as_bytes()))
            .with_context(|| format!("failed to write `{path}`", path = path.display()))?;

        Ok(())
    }
}

impl Default for LockFile {
    fn default() -> Self {
        Self {
            version: LOCK_FILE_VERSION,
            packages: Vec::new(),
        }
    }
}

/// Represents a resolved component dependency.
#[derive(Debug, Default, Clone)]
pub struct DependencyResolution {
    /// The name of the dependency that was resolved.
    pub name: String,
    /// The package resolution of the dependency.
    ///
    /// This is `None` for dependencies specified by path.
    pub package: Option<RegistryPackageResolution>,
    /// The name of the registry that the dependency was resolved from.
    ///
    /// This is `None` for dependencies specified by path or when the
    /// default registry was used.
    pub registry: Option<String>,
    /// The path to the dependency contents.
    pub path: PathBuf,
}

impl DependencyResolution {
    /// The key used in sorting and searching the lock file package list.
    ///
    /// Returns `None` if the dependency is not resolved from a package.
    fn key(&self) -> Option<(&PackageId, Option<&str>)> {
        self.package
            .as_ref()
            .map(|pkg| (&pkg.id, self.registry.as_deref()))
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

        let mut decoded = wit_component::decode(&self.name, &bytes).with_context(|| {
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

        resolve.packages[package].url = self.package.as_ref().map(|p| p.url.to_string());

        Ok(decoded)
    }
}

/// Represents component dependency information about a cargo package.
#[derive(Debug, Clone)]
pub struct PackageDependencyResolution {
    /// The component metadata from the package manifest.
    pub metadata: ComponentMetadata,
    /// The resolved dependencies for the target world.
    pub target_dependencies: HashMap<String, DependencyResolution>,
    /// The resolved dependencies of the component itself.
    pub component_dependencies: HashMap<String, DependencyResolution>,
}

impl PackageDependencyResolution {
    /// Creates a new package dependency resolution for the given package.
    ///
    /// Returns `Ok(None)` if the package does not contain component metadata.
    pub async fn new(
        config: &Config,
        workspace: &Workspace<'_>,
        package: &Package,
        lock_file: &LockFile,
    ) -> Result<Option<Self>> {
        let mut resolution = Self {
            metadata: match ComponentMetadata::from_package(package)? {
                Some(metadata) => metadata,
                None => return Ok(None),
            },
            target_dependencies: Default::default(),
            component_dependencies: Default::default(),
        };

        let target_dependencies = resolution
            .metadata
            .section
            .target
            .as_ref()
            .map(|t| t.dependencies())
            .unwrap_or_default();

        // First create a map of registries to package dependencies from that registry
        let mut map = HashMap::new();
        for (name, dependency, target) in
            target_dependencies.iter().map(|(k, v)| (k, v, true)).chain(
                resolution
                    .metadata
                    .section
                    .dependencies
                    .iter()
                    .map(|(k, v)| (k, v, false)),
            )
        {
            match dependency {
                metadata::Dependency::Package(package) => {
                    let entry = match map.entry(package.registry.as_deref()) {
                        Entry::Occupied(e) => e.into_mut(),
                        Entry::Vacant(e) => {
                            let registry = create(
                                config,
                                e.key().as_deref(),
                                &resolution.metadata.section.registries,
                            )?;
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
                    let dependencies = if target {
                        &mut resolution.target_dependencies
                    } else {
                        &mut resolution.component_dependencies
                    };

                    let prev = dependencies.insert(
                        name.clone(),
                        DependencyResolution {
                            name: name.to_string(),
                            package: None,
                            registry: None,
                            path: p.clone(),
                        },
                    );
                    assert!(prev.is_none());
                }
            }
        }

        // Synchronize each registry if network access is allowed
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
        for (registry_name, RegistryEntry { registry, packages }) in map.into_iter() {
            for (id, entries) in packages {
                for entry in entries {
                    // Resolve the package from the lock file first
                    let res = if let Some(ver) = lock_file.resolve(
                        config,
                        workspace,
                        registry_name.unwrap_or(DEFAULT_REGISTRY_NAME),
                        id,
                        &entry.package.version,
                    )? {
                        let exact_req = VersionReq {
                            comparators: vec![Comparator {
                                op: Op::Exact,
                                major: ver.version.major,
                                minor: Some(ver.version.minor),
                                patch: Some(ver.version.patch),
                                pre: ver.version.pre.clone(),
                            }],
                        };
                        let mut res = registry.resolve(id, &exact_req)?;
                        if let Some(RegistryPackageResolution {
                            requirement,
                            digest,
                            ..
                        }) = &mut res
                        {
                            if digest != &ver.digest {
                                bail!(
                                    "package `{id}` with version `{version}` has digest `{digest}` but the lock file specifies digest `{lock_digest}`",
                                    version = ver.version,
                                    lock_digest = ver.digest,
                                );
                            }

                            // Use the original requirement from the manifest
                            *requirement = entry.package.version.clone();
                        }

                        res
                    } else {
                        registry.resolve(id, &entry.package.version)?
                    };

                    match res {
                        Some(res) => {
                            let path = match &res.location {
                                ContentLocation::Local(path) => path.clone(),
                                ContentLocation::Remote(_) => {
                                    // TODO: check if the contents are in the cache
                                    if !config.cargo().network_allowed() {
                                        bail!("contents of package `{id}` is not in the cache and network access is not allowed");
                                    }
                                    todo!("download package contents")
                                }
                            };

                            let dependencies = if entry.target {
                                &mut resolution.target_dependencies
                            } else {
                                &mut resolution.component_dependencies
                            };

                            dependencies.insert(
                                entry.name.to_string(),
                                DependencyResolution {
                                    name: entry.name.to_string(),
                                    package: Some(res),
                                    registry: registry_name.map(ToOwned::to_owned),
                                    path,
                                },
                            );
                        }
                        None => bail!("a version of package `{id}` that satisfies version requirement `{version}` was not found", version = entry.package.version)
                    }
                }
            }
        }

        return Ok(Some(resolution));

        struct PackageEntry<'a> {
            name: &'a str,
            package: &'a metadata::RegistryPackage,
            target: bool,
        }

        struct RegistryEntry<'a> {
            registry: Box<dyn Registry>,
            packages: HashMap<&'a PackageId, Vec<PackageEntry<'a>>>,
        }
    }

    /// Iterates over all dependencies of the package.
    pub fn deps(&self) -> impl Iterator<Item = (&String, &DependencyResolution)> {
        self.target_dependencies
            .iter()
            .chain(self.component_dependencies.iter())
    }
}

/// Represents a map of cargo packages to their resolved dependencies.
pub type PackageResolutionMap = HashMap<cargo::core::PackageId, PackageDependencyResolution>;
