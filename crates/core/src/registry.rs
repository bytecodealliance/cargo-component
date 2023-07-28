//! Module for resolving dependencies from a component registry.

use crate::{
    lock::{LockFileResolver, LockedPackageVersion},
    progress::{ProgressBar, ProgressStyle},
    terminal::{Colors, Terminal},
};
use anyhow::{bail, Context, Result};
use futures::{stream::FuturesUnordered, StreamExt};
use indexmap::IndexMap;
use semver::{Comparator, Op, Version, VersionReq};
use serde::{
    de::{self, value::MapAccessDeserializer},
    Deserialize, Serialize,
};
use std::{
    collections::{hash_map, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};
use url::Url;
use warg_client::{
    storage::{ContentStorage, PackageInfo, RegistryStorage},
    Config, FileSystemClient, StorageLockResult,
};
use warg_crypto::hash::AnyHash;
use warg_protocol::registry::PackageId;
use wit_component::DecodedWasm;
use wit_parser::{PackageName, Resolve, UnresolvedPackage, WorldId};

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
pub fn create_client(
    config: &warg_client::Config,
    url: &str,
    terminal: &Terminal,
) -> Result<FileSystemClient> {
    match FileSystemClient::try_new_with_config(Some(url), config)? {
        StorageLockResult::Acquired(client) => Ok(client),
        StorageLockResult::NotAcquired(path) => {
            terminal.status_with_color(
                "Blocking",
                format!("waiting for file lock on `{path}`", path = path.display()),
                Colors::Cyan,
            )?;

            Ok(FileSystemClient::new_with_config(Some(url), config)?)
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
                if package.id.is_none() && package.registry.is_none() {
                    let version = package.version.to_string();
                    version.trim_start_matches('^').serialize(serializer)
                } else {
                    #[derive(Serialize)]
                    struct Entry<'a> {
                        package: Option<&'a PackageId>,
                        version: &'a str,
                        registry: Option<&'a str>,
                    }

                    Entry {
                        package: package.id.as_ref(),
                        version: package.version.to_string().trim_start_matches('^'),
                        registry: package.registry.as_deref(),
                    }
                    .serialize(serializer)
                }
            }
            Self::Local(path) => path.serialize(serializer),
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
                    package: Option<PackageId>,
                    version: Option<VersionReq>,
                    registry: Option<String>,
                }

                let entry = Entry::deserialize(MapAccessDeserializer::new(map))?;

                match (entry.path, entry.package, entry.version, entry.registry) {
                    (Some(path), None, None, None) => Ok(Self::Value::Local(path)),
                    (None, id, Some(version), registry) => {
                        Ok(Self::Value::Package(RegistryPackage {
                            id,
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
    /// The id of the package.
    ///
    /// If not specified, the id from the mapping will be used.
    pub id: Option<PackageId>,

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
            id: None,
            version: s.parse()?,
            registry: None,
        })
    }
}

/// Represents information about a resolution of a registry package.
#[derive(Clone, Debug)]
pub struct RegistryResolution {
    /// The id of the dependency that was resolved.
    ///
    /// This may differ from the package id if the dependency was renamed.
    pub id: PackageId,
    /// The id of the package from the registry that was resolved.
    pub package: PackageId,
    /// The name of the registry used to resolve the package.
    ///
    /// A value of `None` indicates that the default registry was used.
    pub registry: Option<String>,
    /// The version requirement that was used to resolve the package.
    pub requirement: VersionReq,
    /// The package version that was resolved.
    pub version: Version,
    /// The digest of the package contents.
    pub digest: AnyHash,
    /// The path to the resolved dependency.
    pub path: PathBuf,
}

/// Represents information about a resolution of a local file.
#[derive(Clone, Debug)]
pub struct LocalResolution {
    /// The id of the dependency that was resolved.
    pub id: PackageId,
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
    /// Gets the id of the dependency that was resolved.
    pub fn id(&self) -> &PackageId {
        match self {
            Self::Registry(res) => &res.id,
            Self::Local(res) => &res.id,
        }
    }

    /// Gets the path to the resolved dependency.
    pub fn path(&self) -> &Path {
        match self {
            Self::Registry(res) => &res.path,
            Self::Local(res) => &res.path,
        }
    }

    /// The key used in sorting and searching the lock file package list.
    ///
    /// Returns `None` if the dependency is not resolved from a registry package.
    pub fn key(&self) -> Option<(&PackageId, Option<&str>)> {
        match self {
            DependencyResolution::Registry(pkg) => Some((&pkg.package, pkg.registry.as_deref())),
            DependencyResolution::Local(_) => None,
        }
    }

    /// Decodes the resolved dependency.
    pub fn decode(&self) -> Result<DecodedDependency> {
        // If the dependency path is a directory, assume it contains wit to parse as a package.
        if self.path().is_dir() {
            return Ok(DecodedDependency::Wit {
                resolution: self,
                package: UnresolvedPackage::parse_dir(self.path()).with_context(|| {
                    format!(
                        "failed to parse dependency `{path}`",
                        path = self.path().display()
                    )
                })?,
            });
        }

        let bytes = fs::read(self.path()).with_context(|| {
            format!(
                "failed to read content of dependency `{id}` at path `{path}`",
                id = self.id(),
                path = self.path().display()
            )
        })?;

        if &bytes[0..4] != b"\0asm" {
            return Ok(DecodedDependency::Wit {
                resolution: self,
                package: UnresolvedPackage::parse(
                    self.path(),
                    std::str::from_utf8(&bytes).with_context(|| {
                        format!(
                            "dependency `{path}` is not UTF-8 encoded",
                            path = self.path().display()
                        )
                    })?,
                )?,
            });
        }

        Ok(DecodedDependency::Wasm {
            resolution: self,
            decoded: wit_component::decode(&bytes).with_context(|| {
                format!(
                    "failed to decode content of dependency `{id}` at path `{path}`",
                    id = self.id(),
                    path = self.path().display()
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
        package: UnresolvedPackage,
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
    pub fn resolve(self) -> Result<(Resolve, wit_parser::PackageId, Vec<PathBuf>)> {
        match self {
            Self::Wit { package, .. } => {
                let mut resolve = Resolve::new();
                let source_files = package.source_files().map(Path::to_path_buf).collect();
                let pkg = resolve.push(package)?;
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
            Self::Wit { package, .. } => &package.name,
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
    terminal: &'a Terminal,
    registry_urls: &'a HashMap<String, Url>,
    warg_config: &'a Config,
    lock_file: Option<LockFileResolver<'a>>,
    registries: IndexMap<&'a str, Registry<'a>>,
    resolutions: HashMap<PackageId, DependencyResolution>,
    network_allowed: bool,
}

impl<'a> DependencyResolver<'a> {
    /// Creates a new dependency resolver.
    pub fn new(
        warg_config: &'a Config,
        registry_urls: &'a HashMap<String, Url>,
        lock_file: Option<LockFileResolver<'a>>,
        terminal: &'a Terminal,
        network_allowed: bool,
    ) -> Result<Self> {
        Ok(DependencyResolver {
            terminal,
            registry_urls,
            warg_config,
            lock_file,
            registries: Default::default(),
            resolutions: Default::default(),
            network_allowed,
        })
    }

    /// Add a dependency to the resolver.
    pub async fn add_dependency(
        &mut self,
        id: &'a PackageId,
        dependency: &'a Dependency,
    ) -> Result<()> {
        match dependency {
            Dependency::Package(package) => {
                // Dependency comes from a registry, add a dependency to the resolver
                let registry_name = package.registry.as_deref().unwrap_or(DEFAULT_REGISTRY_NAME);
                let package_id = package.id.clone().unwrap_or_else(|| id.clone());

                // Resolve the version from the lock file if there is one
                let locked = match self.lock_file.as_ref().and_then(|resolver| {
                    resolver
                        .resolve(registry_name, &package_id, &package.version)
                        .transpose()
                }) {
                    Some(Ok(locked)) => Some(locked),
                    Some(Err(e)) => return Err(e),
                    _ => None,
                };

                let registry = match self.registries.entry(registry_name) {
                    indexmap::map::Entry::Occupied(e) => e.into_mut(),
                    indexmap::map::Entry::Vacant(e) => {
                        let url = find_url(
                            Some(registry_name),
                            self.registry_urls,
                            self.warg_config.default_url.as_deref(),
                        )?;
                        e.insert(Registry {
                            client: Arc::new(create_client(self.warg_config, url, self.terminal)?),
                            packages: HashMap::new(),
                            dependencies: Vec::new(),
                            upserts: HashSet::new(),
                        })
                    }
                };

                registry
                    .add_dependency(id, package_id, &package.version, registry_name, locked)
                    .await?;
            }
            Dependency::Local(p) => {
                // A local path dependency, insert a resolution immediately
                let res = DependencyResolution::Local(LocalResolution {
                    id: id.clone(),
                    path: p.clone(),
                });

                let prev = self.resolutions.insert(id.clone(), res);
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
    pub async fn resolve(self) -> Result<DependencyResolutionMap> {
        let Self {
            mut registries,
            mut resolutions,
            terminal,
            network_allowed,
            ..
        } = self;

        // Start by updating the packages that need updating
        // This will determine the contents that need to be downloaded
        let downloads = Self::update_packages(&mut registries, terminal, network_allowed).await?;

        // Finally, download and resolve the dependencies
        for resolution in
            Self::download_and_resolve(registries, downloads, terminal, network_allowed).await?
        {
            let prev = resolutions.insert(resolution.id().clone(), resolution);
            assert!(prev.is_none());
        }

        Ok(resolutions)
    }

    async fn update_packages(
        registries: &mut IndexMap<&'a str, Registry<'a>>,
        terminal: &Terminal,
        network_allowed: bool,
    ) -> Result<DownloadMap<'a>> {
        let task_count = registries
            .iter()
            .filter(|(_, r)| !r.upserts.is_empty())
            .count();

        let mut progress = ProgressBar::with_style("Updating", ProgressStyle::Ratio, terminal);

        if task_count > 0 {
            if !network_allowed {
                bail!("a component registry update is required but network access is disabled");
            }

            terminal.status("Updating", "component registry package logs")?;
            progress.tick_now(0, task_count, "")?;
        }

        let mut downloads = DownloadMap::new();
        let mut futures = FuturesUnordered::new();
        for (index, (name, registry)) in registries.iter_mut().enumerate() {
            let upserts = std::mem::take(&mut registry.upserts);
            if upserts.is_empty() {
                // No upserts needed, add the necessary downloads now
                registry.add_downloads(name, &mut downloads).await?;
                continue;
            }

            log::info!("updating package logs for registry `{name}`");

            let client = registry.client.clone();
            futures.push(tokio::spawn(async move {
                (index, client.upsert(upserts.iter()).await)
            }))
        }

        assert_eq!(futures.len(), task_count);

        let mut finished = 0;
        while let Some(res) = futures.next().await {
            let (index, res) = res.context("failed to join registry update task")?;
            let (name, registry) = registries
                .get_index_mut(index)
                .expect("out of bounds registry index");

            res.with_context(|| {
                format!("failed to update package logs for component registry `{name}`")
            })?;

            log::info!("package logs successfully updated for component registry `{name}`");
            finished += 1;
            progress.tick_now(finished, task_count, ": updated `{name}`")?;
            registry.add_downloads(name, &mut downloads).await?;
        }

        assert_eq!(finished, task_count);

        progress.clear();

        Ok(downloads)
    }

    async fn download_and_resolve(
        mut registries: IndexMap<&'a str, Registry<'a>>,
        downloads: DownloadMap<'a>,
        terminal: &Terminal,
        network_allowed: bool,
    ) -> Result<impl Iterator<Item = DependencyResolution> + 'a> {
        if !downloads.is_empty() {
            if !network_allowed {
                bail!("a component package download is required but network access is disabled");
            }

            terminal.status("Downloading", "component registry packages")?;

            let mut progress =
                ProgressBar::with_style("Downloading", ProgressStyle::Ratio, terminal);

            let count = downloads.len();
            progress.tick_now(0, count, "")?;

            let mut futures = FuturesUnordered::new();
            for ((registry_name, package, version), deps) in downloads {
                let registry_index = registries.get_index_of(registry_name).unwrap();
                let (_, registry) = registries.get_index(registry_index).unwrap();

                log::info!("downloading content for package `{package}` from component registry `{registry_name}`");

                let client = registry.client.clone();
                futures.push(tokio::spawn(async move {
                    let res = client.download_exact(&package, &version).await;
                    (registry_index, package, version, deps, res)
                }))
            }

            assert_eq!(futures.len(), count);

            let mut finished = 0;
            while let Some(res) = futures.next().await {
                let (registry_index, id, version, deps, res) =
                    res.context("failed to join content download task")?;
                let (name, registry) = registries
                    .get_index_mut(registry_index)
                    .expect("out of bounds registry index");

                let download = res.with_context(|| {
                    format!("failed to download package `{id}` (v{version}) from component registry `{name}`")
                })?;

                log::info!(
                    "downloaded contents of package `{id}` (v{version}) from component registry `{name}`"
                );

                finished += 1;
                progress.tick_now(
                    finished,
                    count,
                    &format!(": downloaded `{id}` (v{version})"),
                )?;

                for index in deps {
                    let dependency = &mut registry.dependencies[index];
                    assert!(dependency.resolution.is_none());
                    dependency.resolution = Some(RegistryResolution {
                        id: dependency.id.clone(),
                        package: dependency.package.clone(),
                        registry: if *name == DEFAULT_REGISTRY_NAME {
                            None
                        } else {
                            Some(name.to_string())
                        },
                        requirement: dependency.version.clone(),
                        version: download.version.clone(),
                        digest: download.digest.clone(),
                        path: download.path.clone(),
                    });
                }
            }

            assert_eq!(finished, count);

            progress.clear();
        }

        Ok(registries
            .into_values()
            .flat_map(|r| r.dependencies.into_iter())
            .map(|d| {
                DependencyResolution::Registry(
                    d.resolution.expect("dependency should have been resolved"),
                )
            }))
    }
}

struct Registry<'a> {
    client: Arc<FileSystemClient>,
    packages: HashMap<PackageId, PackageInfo>,
    dependencies: Vec<RegistryDependency<'a>>,
    upserts: HashSet<PackageId>,
}

impl<'a> Registry<'a> {
    async fn add_dependency(
        &mut self,
        id: &'a PackageId,
        package: PackageId,
        version: &'a VersionReq,
        registry: &str,
        locked: Option<&LockedPackageVersion>,
    ) -> Result<()> {
        let dep = RegistryDependency {
            id,
            package: package.clone(),
            version,
            locked: locked.map(|l| (l.version.clone(), l.digest.clone())),
            resolution: None,
        };

        self.dependencies.push(dep);

        let mut needs_upsert = true;
        if let Some(locked) = locked {
            if let Some(package) =
                Self::load_package(&self.client, &mut self.packages, package.clone()).await?
            {
                if package
                    .state
                    .release(&locked.version)
                    .and_then(|r| r.content())
                    .is_some()
                {
                    // Don't need to upsert this package as it is present
                    // in the lock file and in client storage.
                    needs_upsert = false;
                }
            }
        }

        if needs_upsert && self.upserts.insert(package.clone()) {
            log::info!(
                "package `{package}` from component registry `{registry}` needs to be updated"
            );
        }

        Ok(())
    }

    async fn add_downloads(
        &mut self,
        registry: &'a str,
        downloads: &mut DownloadMap<'a>,
    ) -> Result<()> {
        let Self {
            dependencies,
            packages,
            client,
            ..
        } = self;

        for (index, dependency) in dependencies.iter_mut().enumerate() {
            let package = Self::load_package(client, packages, dependency.package.clone())
                .await?
                .with_context(|| {
                    format!(
                        "package `{name}` was not found in component registry `{registry}`",
                        name = dependency.package
                    )
                })?;

            let release = match &dependency.locked {
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

                    // If an exact match can't be found, fallback to the latest release to
                    // satisfy the version requirement; this can happen when packages are yanked
                    package.state.find_latest_release(&exact_req).map(|r| {
                        // Exact match, verify the content digests match
                        let content = r.content().expect("release must have content");
                        if content != digest {
                            bail!(
                                "component registry package `{name}` (v`{version}`) has digest `{content}` but the lock file specifies digest `{digest}`",
                                name = dependency.package,
                            );
                        }
                        Ok(r)
                    }).transpose()?.or_else(|| package.state.find_latest_release(dependency.version))
                }
                None => package.state.find_latest_release(dependency.version),
            }.with_context(|| format!("component registry package `{name}` has no release matching version requirement `{version}`", name = dependency.package, version = dependency.version))?;

            let digest = release.content().expect("release must have content");
            match client.content().content_location(digest) {
                Some(path) => {
                    // Content is already present, set the resolution
                    assert!(dependency.resolution.is_none());
                    dependency.resolution = Some(RegistryResolution {
                        id: dependency.id.clone(),
                        package: dependency.package.clone(),
                        registry: if registry == DEFAULT_REGISTRY_NAME {
                            None
                        } else {
                            Some(registry.to_string())
                        },
                        requirement: dependency.version.clone(),
                        version: release.version.clone(),
                        digest: digest.clone(),
                        path,
                    });

                    log::info!(
                        "version {version} of registry package `{name}` from registry `{registry}` is already in client storage",
                        name = dependency.package,
                        version = release.version,
                    );
                }
                None => {
                    // Content needs to be downloaded
                    let indexes = downloads
                        .entry((
                            registry,
                            dependency.package.clone(),
                            release.version.clone(),
                        ))
                        .or_default();

                    if indexes.is_empty() {
                        log::info!(
                            "version {version} of registry package `{name}` from registry `{registry}` needs to be downloaded",
                            name = dependency.package,
                            version = release.version,
                        );
                    }

                    indexes.push(index);
                }
            }
        }

        Ok(())
    }

    async fn load_package<'b>(
        client: &FileSystemClient,
        packages: &'b mut HashMap<PackageId, PackageInfo>,
        id: PackageId,
    ) -> Result<Option<&'b PackageInfo>> {
        match packages.entry(id) {
            hash_map::Entry::Occupied(e) => Ok(Some(e.into_mut())),
            hash_map::Entry::Vacant(e) => match client.registry().load_package(e.key()).await? {
                Some(p) => Ok(Some(e.insert(p))),
                None => Ok(None),
            },
        }
    }
}

type DownloadMapKey<'a> = (&'a str, PackageId, Version);
type DownloadMap<'a> = HashMap<DownloadMapKey<'a>, Vec<usize>>;

struct RegistryDependency<'a> {
    /// The package ID assigned in the configuration file.
    id: &'a PackageId,
    /// The package ID of the registry package.
    package: PackageId,
    version: &'a VersionReq,
    locked: Option<(Version, AnyHash)>,
    resolution: Option<RegistryResolution>,
}

/// Represents a map of dependency resolutions.
///
/// The key to the map is the package ID of the dependency.
pub type DependencyResolutionMap = HashMap<PackageId, DependencyResolution>;
