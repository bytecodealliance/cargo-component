//! Module for interacting with component registries.

use crate::{config::Config, metadata};
use anyhow::{anyhow, bail, Context, Result};
use cargo::{
    core::{Package, Workspace},
    util::{Filesystem, Progress, ProgressStyle},
};
use futures::{stream::FuturesUnordered, StreamExt};
use indexmap::IndexMap;
use semver::{Comparator, Op, Version, VersionReq};
use serde::{de::IntoDeserializer, Deserialize, Serialize};
use std::{
    collections::{hash_map, HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use termcolor::Color;
use toml_edit::{Document, Item, Value};
use url::Url;
use warg_client::{
    storage::{ContentStorage, PackageInfo, RegistryStorage},
    FileSystemClient, StorageLockResult,
};
use warg_crypto::hash::AnyHash;
use wit_component::DecodedWasm;

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

/// Finds the URL of the given registry.
///
/// If `name` is `None`, the default registry is used.
pub fn find_url<'a>(
    config: &'a Config,
    name: Option<&'_ str>,
    urls: &'a HashMap<String, Url>,
) -> Result<&'a str> {
    let name = name.unwrap_or(DEFAULT_REGISTRY_NAME);
    match urls.get(name) {
        Some(url) => Ok(url.as_str()),
        None if name != DEFAULT_REGISTRY_NAME => {
            bail!("component registry `{name}` does not exist")
        }
        None => config
            .warg()
            .default_url
            .as_deref()
            .ok_or_else(|| anyhow!("a default registry has not been set")),
    }
}

/// Creates a new registry client for the given url.
///
/// This will attempt to block on the file lock for the registry
/// if it cannot be immediately acquired.
pub fn create_client(config: &Config, url: &str) -> Result<FileSystemClient> {
    match FileSystemClient::try_new_with_config(Some(url), config.warg())? {
        StorageLockResult::Acquired(client) => Ok(client),
        StorageLockResult::NotAcquired(path) => {
            config.shell().status_with_color(
                "Blocking",
                &format!("waiting for file lock on `{path}`", path = path.display()),
                Color::Cyan,
            )?;

            Ok(FileSystemClient::new_with_config(Some(url), config.warg())?)
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
    pub digest: AnyHash,
}

impl LockedPackageVersion {
    pub(crate) fn key(&self) -> &str {
        &self.requirement
    }
}

/// Represents a locked package in a lock file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct LockedPackage {
    /// The name of the locked package.
    pub name: String,
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
    pub(crate) fn key(&self) -> (&str, &str) {
        (
            &self.name,
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
    pub fn from_resolution_map(map: &PackageResolutionMap) -> Self {
        type PackageKey = (String, Option<String>);
        type VersionsMap = HashMap<String, (Version, AnyHash)>;
        let mut packages: HashMap<PackageKey, VersionsMap> = HashMap::new();

        for resolution in map.values() {
            for (_, dep) in resolution.all() {
                match dep.key() {
                    Some((name, registry)) => {
                        let pkg = match dep {
                            DependencyResolution::Registry(pkg) => pkg,
                            DependencyResolution::Local(_) => unreachable!(),
                        };

                        let prev = packages
                            .entry((name.to_string(), registry.map(str::to_string)))
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

        log::info!("opening lock file `{path}`", path = path.display());
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

    /// Updates the lock file on disk given the old lock file to compare against.
    pub fn update(&self, config: &Config, workspace: &Workspace<'_>, old: &Self) -> Result<()> {
        // If the set of packages are the same, we don't need to update the lock file.
        let path = workspace.root().join(LOCK_FILE_NAME);
        if path.is_file() && old == self {
            return Ok(());
        }

        check_lock_update_allowed(config, workspace)?;

        log::info!("updating lock file `{path}`", path = path.display());

        let updated = toml_edit::ser::to_string_pretty(&self)
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

/// Represents information about a resolution of a registry package.
#[derive(Clone, Debug)]
pub struct RegistryResolution {
    /// The id of the dependency that was resolved.
    pub id: metadata::Id,
    /// The name of the package that was resolved.
    pub package: String,
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
    pub id: metadata::Id,
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
    pub fn id(&self) -> &metadata::Id {
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
    fn key(&self) -> Option<(&str, Option<&str>)> {
        match self {
            DependencyResolution::Registry(pkg) => Some((&pkg.package, pkg.registry.as_deref())),
            DependencyResolution::Local(_) => None,
        }
    }

    /// Decodes the resolved dependency.
    pub fn decode(&self) -> Result<DecodedWasm> {
        let bytes = fs::read(self.path()).with_context(|| {
            format!(
                "failed to read content of dependency `{id}` at path `{path}`",
                id = self.id(),
                path = self.path().display()
            )
        })?;

        wit_component::decode(&bytes).with_context(|| {
            format!(
                "failed to decode content of dependency `{id}` at path `{path}`",
                id = self.id(),
                path = self.path().display()
            )
        })
    }
}

/// Represents a map of dependency resolutions.
pub type DependencyResolutionMap = HashMap<metadata::Id, DependencyResolution>;

/// Represents a resolver for a lock file.
#[derive(Clone, Copy, Debug)]
pub struct LockFileResolver<'a> {
    workspace: &'a Workspace<'a>,
    lock_file: &'a LockFile,
}

impl<'a> LockFileResolver<'a> {
    /// Creates a new lock file resolver for the given workspace and lock file.
    pub fn new(workspace: &'a Workspace<'a>, lock_file: &'a LockFile) -> Self {
        Self {
            workspace,
            lock_file,
        }
    }

    /// Resolves a package from the lock file.
    ///
    /// Returns `Ok(None)` if the package cannot be resolved.
    ///
    /// Fails if the package cannot be resolved and the lock file is not allowed to be updated.
    pub fn resolve(
        &'a self,
        config: &Config,
        registry: &str,
        name: &str,
        requirement: &VersionReq,
    ) -> Result<Option<&'a LockedPackageVersion>> {
        if let Some(pkg) = self
            .lock_file
            .packages
            .binary_search_by_key(&(name, registry), LockedPackage::key)
            .ok()
            .map(|i| &self.lock_file.packages[i])
        {
            if let Ok(index) = pkg
                .versions
                .binary_search_by_key(&requirement.to_string().as_str(), LockedPackageVersion::key)
            {
                let locked = &pkg.versions[index];
                log::info!("dependency package `{name}` from registry `{registry}` with requirement `{requirement}` was resolved by the lock file to version {version}", version = locked.version);
                return Ok(Some(locked));
            }
        }

        check_lock_update_allowed(config, self.workspace)?;
        log::info!("dependency package `{name}` from registry `{registry}` with requirement `{requirement}` was not in the lock file");
        Ok(None)
    }
}

/// Represents a resolution of dependencies for a Cargo package.
pub struct PackageDependencyResolution {
    /// The package component metadata.
    pub metadata: metadata::ComponentMetadata,
    /// Resolutions for the package's target dependencies.
    pub target_resolutions: DependencyResolutionMap,
    /// Resolutions for the package's component dependencies.
    pub resolutions: DependencyResolutionMap,
}

impl PackageDependencyResolution {
    /// Creates a new package dependency resolution for the given package.
    ///
    /// Returns `Ok(None)` if the package is not a component package.
    pub async fn new(
        config: &Config,
        package: &Package,
        lock_file: Option<LockFileResolver<'_>>,
    ) -> Result<Option<Self>> {
        let metadata = match metadata::ComponentMetadata::from_package(package)? {
            Some(metadata) => metadata,
            None => return Ok(None),
        };

        let target_deps = metadata
            .section
            .target
            .as_ref()
            .map(|t| t.dependencies())
            .unwrap_or_default();

        let mut resolver = DependencyResolver::new(config, &metadata.section.registries, lock_file);
        for (name, dependency, from_target) in target_deps.iter().map(|(k, v)| (k, v, true)).chain(
            metadata
                .section
                .dependencies
                .iter()
                .map(|(k, v)| (k, v, false)),
        ) {
            resolver
                .add_dependency(name, dependency, from_target)
                .await?;
        }

        let (target_resolutions, resolutions) = resolver.resolve().await?;

        Ok(Some(Self {
            metadata,
            target_resolutions,
            resolutions,
        }))
    }

    /// Iterates over all dependency resolutions of the package.
    pub fn all(&self) -> impl Iterator<Item = (&metadata::Id, &DependencyResolution)> {
        self.target_resolutions
            .iter()
            .chain(self.resolutions.iter())
    }
}

/// Represents a map of cargo packages to their resolved dependencies.
pub type PackageResolutionMap = HashMap<cargo::core::PackageId, PackageDependencyResolution>;

struct RegistryDependency<'a> {
    id: &'a metadata::Id,
    package: String,
    version: &'a VersionReq,
    locked: Option<(Version, AnyHash)>,
    resolution: Option<RegistryResolution>,
    from_target: bool,
}

struct Registry<'a> {
    client: Arc<FileSystemClient>,
    packages: HashMap<String, PackageInfo>,
    dependencies: Vec<RegistryDependency<'a>>,
    upserts: HashSet<String>,
}

impl<'a> Registry<'a> {
    async fn add_dependency(
        &mut self,
        id: &'a metadata::Id,
        package: String,
        version: &'a VersionReq,
        registry: &str,
        locked: Option<&LockedPackageVersion>,
        from_target: bool,
    ) -> Result<()> {
        let dep = RegistryDependency {
            id,
            package: package.clone(),
            version,
            locked: locked.map(|l| (l.version.clone(), l.digest.clone())),
            resolution: None,
            from_target,
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
            log::info!("component registry package `{package}` from registry `{registry}` needs to be updated");
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
                .ok_or_else(|| {
                    anyhow!(
                        "component registry package `{name}` not found in registry `{registry}`",
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
            }.ok_or_else(|| anyhow!("component registry package `{name}` has no release matching version requirement `{version}`", name = dependency.package, version = dependency.version))?;

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
                        "version {version} of component registry package `{name}` from registry `{registry}` is already in client storage",
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
                            "version {version} of component registry package `{name}` from registry `{registry}` needs to be downloaded",
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
        packages: &'b mut HashMap<String, PackageInfo>,
        name: String,
    ) -> Result<Option<&'b PackageInfo>> {
        match packages.entry(name) {
            hash_map::Entry::Occupied(e) => Ok(Some(e.into_mut())),
            hash_map::Entry::Vacant(e) => match client.registry().load_package(e.key()).await? {
                Some(p) => Ok(Some(e.insert(p))),
                None => Ok(None),
            },
        }
    }
}

type DownloadMapKey<'a> = (&'a str, String, Version);
type DownloadMap<'a> = HashMap<DownloadMapKey<'a>, Vec<usize>>;

/// A resolver of package dependencies.
pub struct DependencyResolver<'a> {
    config: &'a Config,
    urls: &'a HashMap<String, Url>,
    registries: IndexMap<&'a str, Registry<'a>>,
    lock_file: Option<LockFileResolver<'a>>,
    target_resolutions: HashMap<metadata::Id, DependencyResolution>,
    resolutions: HashMap<metadata::Id, DependencyResolution>,
}

impl<'a> DependencyResolver<'a> {
    /// Create a new dependency resolver.
    pub fn new(
        config: &'a Config,
        urls: &'a HashMap<String, Url>,
        lock_file: Option<LockFileResolver<'a>>,
    ) -> Self {
        Self {
            config,
            urls,
            registries: IndexMap::new(),
            lock_file,
            target_resolutions: HashMap::new(),
            resolutions: HashMap::new(),
        }
    }

    /// Add a dependency to the resolver.
    pub async fn add_dependency(
        &mut self,
        id: &'a metadata::Id,
        dependency: &'a metadata::Dependency,
        from_target: bool,
    ) -> Result<()> {
        match dependency {
            metadata::Dependency::Package(package) => {
                // Dependency comes from a registry, add a dependency to the resolver
                let registry_name = package.registry.as_deref().unwrap_or(DEFAULT_REGISTRY_NAME);
                let package_name = package.name.clone().unwrap_or_else(|| id.to_string());

                // Resolve the version from the lock file if there is one
                let locked = match self.lock_file.as_ref().and_then(|resolver| {
                    resolver
                        .resolve(self.config, registry_name, &package_name, &package.version)
                        .transpose()
                }) {
                    Some(Ok(locked)) => Some(locked),
                    Some(Err(e)) => return Err(e),
                    _ => None,
                };

                let registry = match self.registries.entry(registry_name) {
                    indexmap::map::Entry::Occupied(e) => e.into_mut(),
                    indexmap::map::Entry::Vacant(e) => {
                        let url = find_url(self.config, Some(registry_name), self.urls)?;
                        e.insert(Registry {
                            client: Arc::new(create_client(self.config, url)?),
                            packages: HashMap::new(),
                            dependencies: Vec::new(),
                            upserts: HashSet::new(),
                        })
                    }
                };

                registry
                    .add_dependency(
                        id,
                        package_name,
                        &package.version,
                        registry_name,
                        locked,
                        from_target,
                    )
                    .await?;
            }
            metadata::Dependency::Local(p) => {
                // A local path dependency, insert a resolution immediately
                let res = DependencyResolution::Local(LocalResolution {
                    id: id.clone(),
                    path: p.clone(),
                });

                let prev = if from_target {
                    self.target_resolutions.insert(id.clone(), res)
                } else {
                    self.resolutions.insert(id.clone(), res)
                };

                assert!(prev.is_none());
            }
        }

        Ok(())
    }

    /// Resolve all dependencies.
    ///
    /// This will download all dependencies that are not already present in client storage.
    ///
    /// Returns a tuple of target dependency resolutions and component dependency resolutions.
    pub async fn resolve(self) -> Result<(DependencyResolutionMap, DependencyResolutionMap)> {
        let Self {
            config,
            mut registries,
            mut target_resolutions,
            mut resolutions,
            ..
        } = self;

        // Start by updating the packages that need updating
        // This will determine the contents that need to be downloaded
        let downloads = Self::update_packages(config, &mut registries).await?;

        // Finally, download and resolve the dependencies
        for (resolution, from_target) in
            Self::download_and_resolve(config, registries, downloads).await?
        {
            let prev = if from_target {
                target_resolutions.insert(resolution.id().clone(), resolution)
            } else {
                resolutions.insert(resolution.id().clone(), resolution)
            };

            assert!(prev.is_none());
        }

        Ok((target_resolutions, resolutions))
    }

    async fn update_packages(
        config: &Config,
        registries: &mut IndexMap<&'a str, Registry<'a>>,
    ) -> Result<DownloadMap<'a>> {
        let task_count = registries
            .iter()
            .filter(|(_, r)| !r.upserts.is_empty())
            .count();

        let mut progress = Progress::with_style("Updating", ProgressStyle::Ratio, config.cargo());

        if task_count > 0 {
            if !config.cargo().network_allowed() {
                bail!("a component registry update is required but network access is disabled");
            }

            config
                .shell()
                .status("Updating", "component registry package logs")?;

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
                (
                    index,
                    client
                        .upsert(&upserts.iter().map(|p| p.as_str()).collect::<Vec<_>>())
                        .await,
                )
            }))
        }

        assert_eq!(futures.len(), task_count);

        let mut finished = 0;
        while let Some(res) = futures.next().await {
            let (index, res) = res.context("failed to join registry update task")?;
            let (name, registry) = registries
                .get_index_mut(index)
                .expect("out of bounds registry index");

            res.with_context(|| format!("failed to update package logs for registry `{name}`"))?;

            log::info!("package logs successfully updated for registry `{name}`");
            finished += 1;
            progress.tick_now(finished, task_count, " updated `{name}`")?;
            registry.add_downloads(name, &mut downloads).await?;
        }

        assert_eq!(finished, task_count);

        progress.clear();

        Ok(downloads)
    }

    async fn download_and_resolve(
        config: &Config,
        mut registries: IndexMap<&'a str, Registry<'a>>,
        downloads: DownloadMap<'a>,
    ) -> Result<impl Iterator<Item = (DependencyResolution, bool)> + 'a> {
        if !downloads.is_empty() {
            if !config.cargo().network_allowed() {
                bail!("a package download is required but network access is disabled");
            }

            config
                .shell()
                .status("Downloading", "component registry packages")?;

            let mut progress =
                Progress::with_style("Downloading", ProgressStyle::Ratio, config.cargo());

            let count = downloads.len();
            progress.tick_now(0, count, "")?;

            let mut futures = FuturesUnordered::new();
            for ((registry_name, package, version), deps) in downloads {
                let registry_index = registries.get_index_of(registry_name).unwrap();
                let (_, registry) = registries.get_index(registry_index).unwrap();

                log::info!("downloading content for component registry package `{package}` from registry `{registry_name}`");

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
                    format!("failed to download package `{id}` (v{version}) from registry `{name}`")
                })?;

                log::info!(
                    "downloaded contents of package `{id}` (v{version}) from registry `{name}`"
                );

                finished += 1;
                progress.tick_now(finished, count, &format!(" downloaded `{id}` (v{version})"))?;

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
                (
                    DependencyResolution::Registry(
                        d.resolution.expect("dependency should have been resolved"),
                    ),
                    d.from_target,
                )
            }))
    }
}
