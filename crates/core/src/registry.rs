//! Module for resolving dependencies from a component registry.
use std::{
    collections::{hash_map, HashMap},
    fmt::Debug,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use anyhow::{bail, Context, Result};
use futures::TryStreamExt;
use indexmap::IndexMap;
use semver::{Comparator, Op, Version, VersionReq};
use serde::{
    de::{self, value::MapAccessDeserializer},
    Deserialize, Serialize,
};

use tokio::io::AsyncReadExt;
use url::Url;
use warg_client::{Config as WargConfig, FileSystemClient, StorageLockResult};
use wasm_pkg_client::{
    caching::{CachingClient, FileCache},
    Client, Config, ContentDigest, Error as WasmPkgError, PackageRef, Release, VersionInfo,
};
use wit_component::DecodedWasm;
use wit_parser::{PackageId, PackageName, Resolve, UnresolvedPackageGroup, WorldId};

use crate::{
    lock::{LockFileResolver, LockedPackageVersion},
    terminal::{Colors, Terminal},
};

/// The name of the default registry.
pub const DEFAULT_REGISTRY_NAME: &str = "default";

/// Finds the URL for the given registry name.
pub fn find_url<'a>(
    name: Option<&str>,
    urls: &'a HashMap<String, Url>,
    default: Option<&'a str>,
) -> Result<&'a str> {
    let name = name.unwrap_or(DEFAULT_REGISTRY_NAME);
    match urls.get(name) {
        Some(url) => Ok(url.as_str()),
        None if name != DEFAULT_REGISTRY_NAME => {
            bail!("component registry `{name}` does not exist in the configuration")
        }
        None => default.context("a default component registry has not been set"),
    }
}

/// Creates a registry client with the given warg configuration.
pub async fn create_client(
    config: &WargConfig,
    url: &str,
    terminal: &Terminal,
) -> Result<FileSystemClient> {
    match FileSystemClient::try_new_with_config(Some(url), config, None).await? {
        StorageLockResult::Acquired(client) => Ok(client),
        StorageLockResult::NotAcquired(path) => {
            terminal.status_with_color(
                "Blocking",
                format!("waiting for file lock on `{path}`", path = path.display()),
                Colors::Cyan,
            )?;

            Ok(FileSystemClient::new_with_config(Some(url), config, None).await?)
        }
    }
}

/// Represents a WIT package dependency.
#[derive(Debug, Clone)]
pub enum Dependency {
    /// The dependency is a registry package.
    Package(RegistryPackage),

    /// The dependency is a path to a local directory or file.
    Local(PathBuf),
}

impl Serialize for Dependency {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Package(package) => {
                if package.name.is_none() && package.registry.is_none() {
                    let version = package.version.to_string();
                    version.trim_start_matches('^').serialize(serializer)
                } else {
                    #[derive(Serialize)]
                    struct Entry<'a> {
                        package: Option<&'a PackageRef>,
                        version: &'a str,
                        registry: Option<&'a str>,
                    }

                    Entry {
                        package: package.name.as_ref(),
                        version: package.version.to_string().trim_start_matches('^'),
                        registry: package.registry.as_deref(),
                    }
                    .serialize(serializer)
                }
            }
            Self::Local(path) => {
                #[derive(Serialize)]
                struct Entry<'a> {
                    path: &'a PathBuf,
                }

                Entry { path }.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Dependency {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Dependency;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(formatter, "a string or a table")
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Self::Value::Package(s.parse().map_err(de::Error::custom)?))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                #[derive(Default, Deserialize)]
                #[serde(default, deny_unknown_fields)]
                struct Entry {
                    path: Option<PathBuf>,
                    package: Option<PackageRef>,
                    version: Option<VersionReq>,
                    registry: Option<String>,
                }

                let entry = Entry::deserialize(MapAccessDeserializer::new(map))?;

                match (entry.path, entry.package, entry.version, entry.registry) {
                    (Some(path), None, None, None) => Ok(Self::Value::Local(path)),
                    (None, name, Some(version), registry) => {
                        Ok(Self::Value::Package(RegistryPackage {
                            name,
                            version,
                            registry,
                        }))
                    }
                    (Some(_), None, Some(_), _) => Err(de::Error::custom(
                        "cannot specify both `path` and `version` fields in a dependency entry",
                    )),
                    (Some(_), None, None, Some(_)) => Err(de::Error::custom(
                        "cannot specify both `path` and `registry` fields in a dependency entry",
                    )),
                    (Some(_), Some(_), _, _) => Err(de::Error::custom(
                        "cannot specify both `path` and `package` fields in a dependency entry",
                    )),
                    (None, None, _, _) => Err(de::Error::missing_field("package")),
                    (None, Some(_), None, _) => Err(de::Error::missing_field("version")),
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

impl FromStr for Dependency {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(Self::Package(s.parse()?))
    }
}

/// Represents a reference to a registry package.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPackage {
    /// The name of the package.
    ///
    /// If not specified, the name from the mapping will be used.
    pub name: Option<PackageRef>,

    /// The version requirement of the package.
    pub version: VersionReq,

    /// The name of the component registry containing the package.
    ///
    /// If not specified, the default registry is used.
    pub registry: Option<String>,
}

impl FromStr for RegistryPackage {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Ok(Self {
            name: None,
            version: s
                .parse()
                .with_context(|| format!("'{s}' is an invalid registry package version"))?,
            registry: None,
        })
    }
}

/// Represents information about a resolution of a registry package.
#[derive(Clone)]
pub struct RegistryResolution {
    /// The name of the dependency that was resolved.
    ///
    /// This may differ from `package` if the dependency was renamed.
    pub name: PackageRef,
    /// The name of the package from the registry that was resolved.
    pub package: PackageRef,
    /// The name of the registry used to resolve the package.
    ///
    /// A value of `None` indicates that the default registry was used.
    pub registry: Option<String>,
    /// The version requirement that was used to resolve the package.
    pub requirement: VersionReq,
    /// The package version that was resolved.
    pub version: Version,
    /// The digest of the package contents.
    pub digest: ContentDigest,
    /// The client to use for fetching the package contents.
    client: Arc<CachingClient<FileCache>>,
}

impl Debug for RegistryResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("RegistryResolution")
            .field("name", &self.name)
            .field("package", &self.package)
            .field("registry", &self.registry)
            .field("requirement", &self.requirement)
            .field("version", &self.version)
            .field("digest", &self.digest)
            .finish()
    }
}

/// Represents information about a resolution of a local file.
#[derive(Clone, Debug)]
pub struct LocalResolution {
    /// The name of the dependency that was resolved.
    pub name: PackageRef,
    /// The path to the resolved dependency.
    pub path: PathBuf,
}

/// Represents a resolution of a dependency.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum DependencyResolution {
    /// The dependency is resolved from a registry package.
    Registry(RegistryResolution),
    /// The dependency is resolved from a local path.
    Local(LocalResolution),
}

impl DependencyResolution {
    /// Gets the name of the dependency that was resolved.
    pub fn name(&self) -> &PackageRef {
        match self {
            Self::Registry(res) => &res.name,
            Self::Local(res) => &res.name,
        }
    }

    /// Gets the resolved version.
    ///
    /// Returns `None` if the dependency is not resolved from a registry package.
    pub fn version(&self) -> Option<&Version> {
        match self {
            Self::Registry(res) => Some(&res.version),
            Self::Local(_) => None,
        }
    }

    /// The key used in sorting and searching the lock file package list.
    ///
    /// Returns `None` if the dependency is not resolved from a registry package.
    pub fn key(&self) -> Option<(&PackageRef, Option<&str>)> {
        match self {
            DependencyResolution::Registry(pkg) => Some((&pkg.package, pkg.registry.as_deref())),
            DependencyResolution::Local(_) => None,
        }
    }

    /// Decodes the resolved dependency.
    pub async fn decode(&self) -> Result<DecodedDependency> {
        // If the dependency path is a directory, assume it contains wit to parse as a package.
        let bytes = match self {
            DependencyResolution::Local(LocalResolution { path, .. })
                if tokio::fs::metadata(path).await?.is_dir() =>
            {
                return Ok(DecodedDependency::Wit {
                    resolution: self,
                    package: UnresolvedPackageGroup::parse_dir(path).with_context(|| {
                        format!("failed to parse dependency `{path}`", path = path.display())
                    })?,
                });
            }
            DependencyResolution::Local(LocalResolution { path, .. }) => {
                tokio::fs::read(path).await.with_context(|| {
                    format!(
                        "failed to read content of dependency `{name}` at path `{path}`",
                        name = self.name(),
                        path = path.display()
                    )
                })?
            }
            DependencyResolution::Registry(res) => {
                let stream = res
                    .client
                    .get_content(
                        &res.package,
                        &Release {
                            version: res.version.clone(),
                            content_digest: res.digest.clone(),
                        },
                    )
                    .await?;

                let mut buf = Vec::new();
                tokio_util::io::StreamReader::new(
                    stream.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
                )
                .read_to_end(&mut buf)
                .await?;
                buf
            }
        };

        if &bytes[0..4] != b"\0asm" {
            return Ok(DecodedDependency::Wit {
                resolution: self,
                package: UnresolvedPackageGroup::parse(
                    // This is fake, but it's needed for the parser to work.
                    self.name().to_string(),
                    std::str::from_utf8(&bytes).with_context(|| {
                        format!(
                            "dependency `{name}` is not UTF-8 encoded",
                            name = self.name()
                        )
                    })?,
                )?,
            });
        }

        Ok(DecodedDependency::Wasm {
            resolution: self,
            decoded: wit_component::decode(&bytes).with_context(|| {
                format!(
                    "failed to decode content of dependency `{name}`",
                    name = self.name(),
                )
            })?,
        })
    }
}

/// Represents a decoded dependency.
pub enum DecodedDependency<'a> {
    /// The dependency decoded from an unresolved WIT package.
    Wit {
        /// The resolution related to the decoded dependency.
        resolution: &'a DependencyResolution,
        /// The unresolved WIT package.
        package: UnresolvedPackageGroup,
    },
    /// The dependency decoded from a Wasm file.
    Wasm {
        /// The resolution related to the decoded dependency.
        resolution: &'a DependencyResolution,
        /// The decoded Wasm file.
        decoded: DecodedWasm,
    },
}

impl<'a> DecodedDependency<'a> {
    /// Fully resolves the dependency.
    ///
    /// If the dependency is an unresolved WIT package, it will assume that the
    /// package has no foreign dependencies.
    pub fn resolve(self) -> Result<(Resolve, PackageId, Vec<PathBuf>)> {
        match self {
            Self::Wit { package, .. } => {
                let mut resolve = Resolve::new();
                let source_files = package
                    .source_map
                    .source_files()
                    .map(Path::to_path_buf)
                    .collect();
                let pkg = resolve.push_group(package)?;
                Ok((resolve, pkg, source_files))
            }
            Self::Wasm { decoded, .. } => match decoded {
                DecodedWasm::WitPackage(resolve, pkg) => Ok((resolve, pkg, Vec::new())),
                DecodedWasm::Component(resolve, world) => {
                    let pkg = resolve.worlds[world].package.unwrap();
                    Ok((resolve, pkg, Vec::new()))
                }
            },
        }
    }

    /// Gets the package name of the decoded dependency.
    pub fn package_name(&self) -> &PackageName {
        match self {
            Self::Wit { package, .. } => &package.main.name,
            Self::Wasm { decoded, .. } => &decoded.resolve().packages[decoded.package()].name,
        }
    }

    /// Converts the decoded dependency into a component world.
    ///
    /// Returns an error if the dependency is not a decoded component.
    pub fn into_component_world(self) -> Result<(Resolve, WorldId)> {
        match self {
            Self::Wasm {
                decoded: DecodedWasm::Component(resolve, world),
                ..
            } => Ok((resolve, world)),
            _ => bail!("dependency is not a WebAssembly component"),
        }
    }
}

/// Used to resolve dependencies for a WIT package.
pub struct DependencyResolver<'a> {
    client: Arc<CachingClient<FileCache>>,
    lock_file: Option<LockFileResolver<'a>>,
    registries: IndexMap<&'a str, Registry<'a>>,
    resolutions: HashMap<PackageRef, DependencyResolution>,
}

impl<'a> DependencyResolver<'a> {
    /// Creates a new dependency resolver. If `config` is `None`, then the resolver will be set to
    /// offline mode and a lock file must be given as well. Anything that will require network
    /// access will fail in offline mode.
    pub fn new(
        config: Option<Config>,
        lock_file: Option<LockFileResolver<'a>>,
        cache: FileCache,
    ) -> anyhow::Result<Self> {
        if config.is_none() && lock_file.is_none() {
            anyhow::bail!("lock file must be provided when offline mode is enabled");
        }
        let client = CachingClient::new(config.map(Client::new), cache);
        Ok(DependencyResolver {
            client: Arc::new(client),
            lock_file,
            registries: Default::default(),
            resolutions: Default::default(),
        })
    }

    /// Creates a new dependency resolver with the given client. This is useful when you already
    /// have a client available. If the client is set to offline mode, then a lock file must be
    /// given or this will error
    pub fn new_with_client(
        client: Arc<CachingClient<FileCache>>,
        lock_file: Option<LockFileResolver<'a>>,
    ) -> anyhow::Result<Self> {
        if client.is_readonly() && lock_file.is_none() {
            anyhow::bail!("lock file must be provided when offline mode is enabled");
        }
        Ok(DependencyResolver {
            client,
            lock_file,
            registries: Default::default(),
            resolutions: Default::default(),
        })
    }

    /// Add a dependency to the resolver.
    pub async fn add_dependency(
        &mut self,
        name: &'a PackageRef,
        dependency: &'a Dependency,
    ) -> Result<()> {
        match dependency {
            Dependency::Package(package) => {
                // Dependency comes from a registry, add a dependency to the resolver
                let registry_name = package.registry.as_deref().unwrap_or(DEFAULT_REGISTRY_NAME);
                let package_name = package.name.clone().unwrap_or_else(|| name.clone());

                // Resolve the version from the lock file if there is one
                let locked = match self.lock_file.as_ref().and_then(|resolver| {
                    resolver
                        .resolve(registry_name, &package_name, &package.version)
                        .transpose()
                }) {
                    Some(Ok(locked)) => Some(locked),
                    Some(Err(e)) => return Err(e),
                    _ => None,
                };

                let registry = match self.registries.entry(registry_name) {
                    indexmap::map::Entry::Occupied(e) => e.into_mut(),
                    indexmap::map::Entry::Vacant(e) => e.insert(Registry {
                        client: self.client.clone(),
                        packages: HashMap::new(),
                        dependencies: Vec::new(),
                    }),
                };

                registry
                    .add_dependency(name, package_name, &package.version, locked)
                    .await?;
            }
            Dependency::Local(p) => {
                // A local path dependency, insert a resolution immediately
                let res = DependencyResolution::Local(LocalResolution {
                    name: name.clone(),
                    path: p.clone(),
                });

                let prev = self.resolutions.insert(name.clone(), res);
                assert!(prev.is_none());
            }
        }

        Ok(())
    }

    /// Resolve all dependencies.
    ///
    /// This will download all dependencies that are not already present in client storage.
    ///
    /// Returns the dependency resolution map.
    pub async fn resolve(mut self) -> Result<DependencyResolutionMap> {
        // Resolve all dependencies
        for (name, registry) in self.registries.iter_mut() {
            registry.resolve(name).await?;
        }

        for resolution in self
            .registries
            .into_values()
            .flat_map(|r| r.dependencies.into_iter())
            .map(|d| {
                DependencyResolution::Registry(
                    d.resolution.expect("dependency should have been resolved"),
                )
            })
        {
            let prev = self
                .resolutions
                .insert(resolution.name().clone(), resolution);
            assert!(prev.is_none());
        }

        Ok(self.resolutions)
    }
}

struct Registry<'a> {
    client: Arc<CachingClient<FileCache>>,
    packages: HashMap<PackageRef, Vec<VersionInfo>>,
    dependencies: Vec<RegistryDependency<'a>>,
}

impl<'a> Registry<'a> {
    async fn add_dependency(
        &mut self,
        name: &'a PackageRef,
        package: PackageRef,
        version: &'a VersionReq,
        locked: Option<&LockedPackageVersion>,
    ) -> Result<()> {
        let dep = RegistryDependency {
            name,
            package: package.clone(),
            version,
            locked: locked.map(|l| (l.version.clone(), l.digest.clone())),
            resolution: None,
        };

        self.dependencies.push(dep);

        Ok(())
    }

    async fn resolve(&mut self, registry: &'a str) -> Result<()> {
        for dependency in self.dependencies.iter_mut() {
            // We need to clone a handle to the client because we mutably borrow self below. Might
            // be worth replacing the mutable borrow with a RwLock down the line.
            let client = self.client.clone();

            let (selected_version, digest) = if client.is_readonly() {
                dependency
                    .locked
                    .as_ref()
                    .map(|(ver, digest)| (ver, Some(digest)))
                    .ok_or_else(|| {
                        anyhow::anyhow!("Couldn't find locked dependency while in offline mode")
                    })?
            } else {
                let versions =
                    load_package(&mut self.packages, &self.client, dependency.package.clone())
                        .await?
                        .with_context(|| {
                            format!(
                                "package `{name}` was not found in component registry `{registry}`",
                                name = dependency.package
                            )
                        })?;

                match &dependency.locked {
                    Some((version, digest)) => {
                        // The dependency had a lock file entry, so attempt to do an exact match first
                        let exact_req = VersionReq {
                            comparators: vec![Comparator {
                                op: Op::Exact,
                                major: version.major,
                                minor: Some(version.minor),
                                patch: Some(version.patch),
                                pre: version.pre.clone(),
                            }],
                        };

                        // If an exact match can't be found, fallback to the latest release to satisfy
                        // the version requirement; this can happen when packages are yanked. If we did
                        // find an exact match, return the digest for comparison after fetching the
                        // release
                        find_latest_release(versions, &exact_req).map(|v| (&v.version, Some(digest))).or_else(|| find_latest_release(versions, dependency.version).map(|v| (&v.version, None)))
                    }
                    None => find_latest_release(versions, dependency.version).map(|v| (&v.version, None)),
                }.with_context(|| format!("component registry package `{name}` has no release matching version requirement `{version}`", name = dependency.package, version = dependency.version))?
            };

            // We need to clone a handle to the client because we mutably borrow self above. Might
            // be worth replacing the mutable borrow with a RwLock down the line.
            let release = client
                .get_release(&dependency.package, selected_version)
                .await?;
            if let Some(digest) = digest {
                if &release.content_digest != digest {
                    bail!(
                        "component registry package `{name}` (v`{version}`) has digest `{content}` but the lock file specifies digest `{digest}`",
                        name = dependency.package,
                        version = release.version,
                        content = release.content_digest,
                    );
                }
            }

            dependency.resolution = Some(RegistryResolution {
                name: dependency.name.clone(),
                package: dependency.package.clone(),
                registry: if registry == DEFAULT_REGISTRY_NAME {
                    None
                } else {
                    Some(registry.to_string())
                },
                requirement: dependency.version.clone(),
                version: release.version.clone(),
                digest: release.content_digest.clone(),
                client: self.client.clone(),
            });
        }

        Ok(())
    }
}

async fn load_package<'b>(
    packages: &'b mut HashMap<PackageRef, Vec<VersionInfo>>,
    client: &CachingClient<FileCache>,
    package: PackageRef,
) -> Result<Option<&'b Vec<VersionInfo>>> {
    match packages.entry(package) {
        hash_map::Entry::Occupied(e) => Ok(Some(e.into_mut())),
        hash_map::Entry::Vacant(e) => match client.list_all_versions(e.key()).await {
            Ok(p) => Ok(Some(e.insert(p))),
            Err(WasmPkgError::PackageNotFound) => Ok(None),
            Err(err) => Err(err.into()),
        },
    }
}

struct RegistryDependency<'a> {
    /// The package name assigned in the configuration file.
    name: &'a PackageRef,
    /// The package name of the registry package.
    package: PackageRef,
    version: &'a VersionReq,
    locked: Option<(Version, ContentDigest)>,
    resolution: Option<RegistryResolution>,
}

/// Represents a map of dependency resolutions.
///
/// The key to the map is the package name of the dependency.
pub type DependencyResolutionMap = HashMap<PackageRef, DependencyResolution>;

fn find_latest_release<'a>(
    versions: &'a [VersionInfo],
    req: &VersionReq,
) -> Option<&'a VersionInfo> {
    versions
        .iter()
        .filter(|info| !info.yanked && req.matches(&info.version))
        .max_by(|a, b| a.version.cmp(&b.version))
}
