use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Args;

use cargo_component_core::{command::CommonOptions, VersionedPackageName};
use futures::TryStreamExt;
use tokio_util::io::{StreamReader, SyncIoBridge};
use warg_loader::{ClientConfig, Release};
use wit_component::DecodedWasm;
use wit_parser::{PackageId, PackageName, Resolve, UnresolvedPackage};

/// Pull WIT package(s) to a local "deps" directory.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct PullCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Use the specified registry name as the default when pulling package(s).
    /// Note: Currently 'wasi:' packages will be pulled from
    /// 'bytecodealliance.org' regardless of this flag.
    #[clap(long, value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// Update the specified directory WIT "root" directory. Dependencies will
    /// be written to e.g. "<wit-dir>/deps/<namespace>.<package>.wit".
    #[clap(long, value_name = "WIT_DIR", default_value = "wit")]
    pub wit_dir: PathBuf,

    /// Pull the packages specified. If empty, the list of packages to pull
    /// will be inferred from missing dependencies.
    #[clap(value_name = "PACKAGE")]
    pub packages: Vec<VersionedPackageName>,
}

impl PullCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing pull command");

        let terminal = self.common.new_terminal();

        let mut pkgs_state = PackagesState::parse_dir(&self.wit_dir)?;
        log::debug!("Packages state: {pkgs_state:?}");

        // Determine set of packages to pull
        let packages = if self.packages.is_empty() {
            // Warn on unparsable root package; might unexpectedly be missing deps
            if let Err(err) = UnresolvedPackage::parse_dir(&self.wit_dir) {
                terminal.warn(format!("Couldn't parse root package: {err}"))?;
            }
            // No packages specified; pull missing dependencies
            pkgs_state
                .missing_deps()
                .map(|pkg| {
                    let name = format!("{}:{}", pkg.namespace, pkg.name).parse().unwrap();
                    let version = pkg
                        .version
                        .as_ref()
                        .map(|ver| ver.to_string().parse().unwrap());
                    VersionedPackageName { name, version }
                })
                .collect::<Vec<_>>()
        } else {
            // Remove existing packages from given list
            self.packages
                .iter()
                .filter(|pkg| !pkgs_state.satisfies(pkg))
                .cloned()
                .collect()
        };

        if packages.is_empty() {
            terminal.status("Finished", "no missing packages; nothing to do")?;
            return Ok(());
        }
        log::debug!("Packages to pull: {packages:?}");

        let mut client = {
            let mut config = ClientConfig::default();
            config.namespace_registry("wasi", "bytecodealliance.org");
            if let Some(file_config) = ClientConfig::from_default_file()? {
                config.merge_config(file_config);
            }
            if let Some(registry) = self.registry.clone() {
                config.default_registry(registry);
            }
            config.to_client()
        };

        for pkg in packages {
            if pkgs_state.satisfies(&pkg) {
                log::info!("Skipping {pkg}; resolved by previous pull?");
                continue;
            }
            terminal.status("Resolving", format!("package {pkg}"))?;

            match self.pull(&mut client, &pkg).await? {
                Some((release, decoded)) => {
                    let root_pkg = &decoded.resolve().packages[decoded.package()];

                    let name = &pkg.name;
                    let release_pkg = PackageName {
                        namespace: name.namespace().to_string(),
                        name: name.name().to_string(),
                        version: Some(release.version.clone()),
                    };
                    terminal.status("Downloaded", format!("release {release_pkg}"))?;

                    if let Some(wit_version) = &root_pkg.name.version {
                        let release_version = &release.version;
                        if wit_version != release_version {
                            terminal.warn(format!("Release version {release_version} doesn't match WIT package version {wit_version}"))?;
                        }
                    }

                    for (package_id, package) in &decoded.resolve().packages {
                        let name = &package.name;
                        if !pkgs_state.insert(name.clone()) {
                            continue;
                        }
                        let path = self.write_package(decoded.resolve(), package_id)?;
                        terminal.status(
                            "Wrote",
                            format!("package {name} to '{path}'", path = path.display()),
                        )?;
                    }
                }
                None => {
                    terminal.warn(format!("No package found for {pkg}"))?;
                }
            }
        }
        Ok(())
    }

    async fn pull(
        &self,
        client: &mut warg_loader::Client,
        versioned_pkg: &VersionedPackageName,
    ) -> Result<Option<(Release, DecodedWasm)>> {
        let pkg_ref = versioned_pkg.name.to_string().parse()?;

        let versions = client.list_all_versions(&pkg_ref).await?;
        let Some(version) = versions
            .into_iter()
            .filter(|version| {
                if let Some(expected) = &versioned_pkg.version {
                    expected.matches(version)
                } else {
                    true
                }
            })
            .max()
        else {
            return Ok(None);
        };
        log::debug!("Resolved {versioned_pkg} to version {version}");

        let release = client.get_release(&pkg_ref, &version).await?;

        let stream = client.stream_content(&pkg_ref, &release).await?;
        let stream = StreamReader::new(stream.map_err(|err| match err {
            warg_loader::Error::IoError(err) => err,
            other => std::io::Error::other(other),
        }));
        let reader = SyncIoBridge::new(stream);

        let decoded = tokio::task::block_in_place(|| wit_component::decode_reader(reader))?;

        Ok(Some((release, decoded)))
    }

    fn write_package(&self, resolve: &Resolve, package_id: PackageId) -> Result<PathBuf> {
        let wit = wit_component::WitPrinter::default().print(resolve, package_id)?;
        let pkg = &resolve.packages[package_id];

        let file_version = pkg
            .name
            .version
            .as_ref()
            .map(|version| format!("@{version}"))
            .unwrap_or_default();
        let file_name = format!(
            "{namespace}-{name}{file_version}.wit",
            namespace = pkg.name.namespace,
            name = pkg.name.name,
        );
        let path = self.ensure_deps_dir()?.join(file_name);
        log::debug!("Writing to {path:?}");

        let mut file =
            tempfile::NamedTempFile::with_prefix_in(".tmp-wit-pull-", path.parent().unwrap())?;
        file.write_all(wit.as_bytes())?;
        file.persist_noclobber(&path)
            .or_else(|err| Err(err.error).context(format!("Failed to write {path:?}")))?;

        Ok(path)
    }

    fn ensure_deps_dir(&self) -> Result<PathBuf> {
        let path = self.wit_dir.join("deps");
        if !path.exists() {
            std::fs::create_dir(&path).with_context(|| format!("couldn't create {path:?}"))?;
        }
        Ok(path)
    }
}

struct PackagesState {
    // Packages currently present in the wit dir
    present: BTreeSet<PackageName>,
    // All deps, present or not
    deps: BTreeSet<PackageName>,
}

impl PackagesState {
    fn parse_dir(wit_dir: &Path) -> Result<Self> {
        let mut resolve = Resolve::new();

        // If Resolve::push_dir succeeds there are no unresolved deps
        match resolve.push_dir(wit_dir) {
            Ok(_) => {
                return Ok(Self {
                    present: resolve.package_names.into_keys().collect(),
                    deps: Default::default(),
                })
            }
            Err(err) => log::debug!("Couldn't resolve packages: {err:#}"),
        }

        let mut present = BTreeSet::new();
        let mut deps = BTreeSet::new();

        // Root package
        match UnresolvedPackage::parse_dir(wit_dir) {
            Ok(pkg) => {
                log::debug!(
                    "Parsed root package '{name}' deps = {deps:?}",
                    name = pkg.name,
                    deps = debug_pkg_names(pkg.foreign_deps.keys()),
                );
                present.insert(pkg.name);
                deps.extend(pkg.foreign_deps.into_keys());
            }
            Err(err) => {
                log::debug!("Couldn't parse root package: {err:?}");
            }
        };

        // Approximate the Resolve::push_dir process
        // TODO: Try to expose this logic from wit-parser instead
        let deps_dir = wit_dir.join("deps");
        if deps_dir.is_dir() {
            for entry in deps_dir.read_dir()? {
                let entry = entry?;
                let path = entry.path();
                let pkg = if entry.file_type()?.is_dir() {
                    UnresolvedPackage::parse_dir(&path)
                } else if path.extension().unwrap_or_default() == "wit" {
                    UnresolvedPackage::parse_file(&path)
                } else {
                    continue;
                }
                .with_context(|| format!("Error parsing {path:?}"))?;

                log::debug!(
                    "Parsed deps package '{name}' deps = {deps:?}",
                    name = pkg.name,
                    deps = debug_pkg_names(pkg.foreign_deps.keys()),
                );
                present.insert(pkg.name);
                deps.extend(pkg.foreign_deps.into_keys());
            }
        }
        Ok(Self { present, deps })
    }

    fn insert(&mut self, name: PackageName) -> bool {
        self.present.insert(name)
    }

    fn missing_deps(&self) -> impl Iterator<Item = &PackageName> {
        self.deps.difference(&self.present)
    }

    fn satisfies(&self, need: &VersionedPackageName) -> bool {
        let need_name = &need.name;
        for candidate in &self.present {
            if need_name.namespace() != candidate.namespace || need_name.name() != candidate.name {
                continue;
            }
            match (&need.version, &candidate.version) {
                // Requested version satisfied by candidate?
                (Some(req), Some(ver)) if req.matches(ver) => return true,
                // No requested version; any matching name works
                (None, _) => return true,
                _ => (),
            }
        }
        false
    }
}

impl std::fmt::Debug for PackagesState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackagesState")
            .field("present", &debug_pkg_names(&self.present))
            .field("deps", &debug_pkg_names(&self.deps))
            .finish()
    }
}

fn debug_pkg_names<'a>(names: impl IntoIterator<Item = &'a PackageName>) -> Vec<String> {
    names
        .into_iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
}
