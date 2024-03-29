use std::{
    collections::{BTreeSet, HashSet},
    io::Write,
    path::PathBuf,
};

use anyhow::{bail, Context, Result};
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

    /// Use the specified registry name when pulling the package(s).
    #[clap(long, value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// Update the specified directory WIT "root" directory. Dependencies will
    /// be written to e.g. "<wit-dir>/deps/<namespace>.<package>.wit".
    #[clap(long, value_name = "WIT_DIR", default_value = "wit")]
    pub wit_dir: PathBuf,

    /// Create "<wit-dir>" and "<wit-dir>/deps" directories if needed.
    #[clap(long)]
    pub create_dirs: bool,

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

        let (mut existing_pkgs, deps) = self.parse_packages_state()?;
        log::debug!("Packages state: pkgs={existing_pkgs:?} deps={deps:?}");

        let packages = if self.packages.is_empty() {
            // No packages specified; look for missing dependencies
            deps.into_iter()
                .filter(|pkg| !existing_pkgs.contains(pkg))
                .map(|pkg| VersionedPackageName {
                    name: wit_to_warg_package_name(&pkg),
                    version: pkg.version.map(|ver| ver.to_string().parse().unwrap()),
                })
                .collect::<Vec<_>>()
        } else {
            // Remove existing packages from given list
            self.packages
                .iter()
                .filter(|pkg| {
                    !existing_pkgs
                        .iter()
                        .any(|existing| version_satisfied(pkg, existing))
                })
                .cloned()
                .collect()
        };

        if packages.is_empty() {
            terminal.status("Finished", "no missing packages; nothing to do")?;
            return Ok(());
        }

        let mut client = {
            let mut config = ClientConfig::default();
            config.namespace_registry("wasi", "bytecodealliance.org");
            if let Some(file_config) = ClientConfig::from_default_file()? {
                config.merge_config(file_config);
            }
            config.to_client()
        };

        let mut pulled = HashSet::new();
        for pkg in packages {
            let name = &pkg.name;
            if !pulled.insert(name.clone()) {
                continue;
            }
            terminal.status("Resolving", format!("package {pkg}"))?;

            match self.pull(&mut client, &pkg).await? {
                Some((release, decoded)) => {
                    let root_pkg = &decoded.resolve().packages[decoded.package()];

                    let release_pkg = PackageName {
                        namespace: name.namespace().to_string(),
                        name: name.name().to_string(),
                        version: Some(release.version.clone()),
                    };
                    terminal.status("Downloaded", format!("release {release_pkg}"))?;

                    let version = root_pkg.name.version.clone();
                    if let Some(wit_version) = &version {
                        let release_version = &release.version;
                        if wit_version != release_version {
                            terminal.warn(format!("Release version {release_version} doesn't match WIT package version {wit_version}"))?;
                        }
                    }

                    for (package_id, package) in &decoded.resolve().packages {
                        let name = &package.name;
                        if !existing_pkgs.insert(name.clone()) {
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

    fn parse_packages_state(&self) -> Result<(BTreeSet<PackageName>, BTreeSet<PackageName>)> {
        let mut resolve = Resolve::new();

        // If Resolve::push_dir succeeds there are no unresolved deps
        match resolve.push_dir(&self.wit_dir) {
            Ok(_) => {
                return Ok((
                    resolve.package_names.into_keys().collect(),
                    Default::default(),
                ))
            }
            Err(err) => log::debug!("Couldn't resolve packages: {err:#}"),
        }

        // Approximate the Resolve::push_dir process
        // TODO: Try to expose this logic from wit-parser instead
        let mut pkgs = match UnresolvedPackage::parse_dir(&self.wit_dir) {
            Ok(pkg) => BTreeSet::from([pkg.name]),
            Err(err) => {
                log::debug!("Couldn't parse root package: {err:?}");
                Default::default()
            }
        };
        let mut deps = BTreeSet::new();
        let deps_dir = self.wit_dir.join("deps");
        if deps_dir.is_dir() {
            for entry in deps_dir.read_dir()? {
                let path = entry?.path();
                if path.extension().unwrap_or_default() != "wit" {
                    continue;
                }
                let pkg = UnresolvedPackage::parse_file(&path)
                    .with_context(|| format!("Error parsing {path:?}"))?;
                pkgs.insert(pkg.name);
                deps.extend(pkg.foreign_deps.into_keys());
            }
        }
        Ok((pkgs, deps))
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
            .map(
                |version| match (version.major, version.minor, version.patch) {
                    (0, 0, patch) => format!("@0.0.{patch}"),
                    (0, minor, _) => format!("@0.{minor}"),
                    (major, _, _) => format!("@{major}"),
                },
            )
            .unwrap_or_default();
        let file_name = format!(
            "{namespace}-{name}{file_version}.wit",
            namespace = pkg.name.namespace,
            name = pkg.name.name,
        );
        let path = self.deps_dir()?.join(file_name);
        log::debug!("Writing to {path:?}");

        let mut file =
            tempfile::NamedTempFile::with_prefix_in(".tmp-wit-pull-", path.parent().unwrap())?;
        file.write_all(wit.as_bytes())?;
        file.persist_noclobber(&path)
            .or_else(|err| Err(err.error).context(format!("Failed to write {path:?}")))?;

        Ok(path)
    }

    fn deps_dir(&self) -> Result<PathBuf> {
        let path = self.wit_dir.join("deps");
        if !path.exists() {
            if self.create_dirs {
                log::info!("Creating {path:?}");
                std::fs::create_dir_all(&path)?;
            } else {
                bail!("Deps dir does not exist at {path:?}");
            }
        }
        Ok(path)
    }
}

fn version_satisfied(expected: &VersionedPackageName, actual: &PackageName) -> bool {
    if expected.name.namespace() != actual.namespace || expected.name.name() != actual.name {
        return false;
    }
    match (&expected.version, &actual.version) {
        (Some(expected), Some(actual)) => expected.matches(actual),
        (None, _) => true,
        _ => false,
    }
}

fn wit_to_warg_package_name(wit: &PackageName) -> warg_protocol::registry::PackageName {
    format!("{}:{}", wit.namespace, wit.name).parse().unwrap()
}
