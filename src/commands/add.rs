use crate::{
    config::{CargoArguments, CargoPackageSpec},
    load_component_metadata, load_metadata,
    metadata::ComponentMetadata,
    Config, PackageComponentMetadata,
};
use anyhow::{bail, Context, Result};
use cargo_component_core::{
    registry::{Dependency, DependencyResolution, DependencyResolver, RegistryPackage},
    VersionedPackageId,
};
use cargo_metadata::Package;
use clap::{ArgAction, Args};
use semver::VersionReq;
use std::{fs, path::PathBuf};
use toml_edit::{value, Document, InlineTable, Value};
use warg_protocol::registry::PackageId;

/// Add a dependency for a WebAssembly component
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct AddCommand {
    /// Do not print cargo log messages
    #[clap(long = "quiet", short = 'q')]
    pub quiet: bool,

    /// Use verbose output (-vv very verbose/build.rs output)
    #[clap(
        long = "verbose",
        short = 'v',
        action = ArgAction::Count
    )]
    pub verbose: u8,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

    /// Path to the manifest to add a dependency to
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Don't actually write the manifest
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// Cargo package to add the dependency to (see `cargo help pkgid`)
    #[clap(long = "package", short = 'p', value_name = "SPEC")]
    pub spec: Option<CargoPackageSpec>,

    /// The name of the registry to use.
    #[clap(long = "registry", short = 'r', value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// The id of the dependency to use; defaults to the package id.
    #[clap(long, value_name = "ID")]
    pub id: Option<PackageId>,

    /// The id of the package to add a dependency to.
    #[clap(value_name = "PACKAGE")]
    pub package: VersionedPackageId,
}

impl AddCommand {
    /// Executes the command
    pub async fn exec(self, config: &Config, cargo_args: &CargoArguments) -> Result<()> {
        let metadata = load_metadata(cargo_args.manifest_path.as_deref())?;

        let PackageComponentMetadata { package, metadata }: PackageComponentMetadata<'_> =
            match &self.spec {
                Some(spec) => {
                    let pkgs = load_component_metadata(&metadata, std::iter::once(spec), false)?;
                    assert!(pkgs.len() == 1, "one package should be present");
                    pkgs.into_iter().next().unwrap()
                }
                None => PackageComponentMetadata::new(
                    metadata
                        .root_package()
                        .context("no root package found in metadata")?,
                )?,
            };

        let metadata = metadata.with_context(|| {
            format!(
                "manifest `{path}` is not a WebAssembly component package",
                path = package.manifest_path
            )
        })?;

        let id = match &self.id {
            Some(id) => id,
            None => &self.package.id,
        };

        self.validate(&metadata, id)?;

        let version = self
            .resolve_version(config, &metadata, id, cargo_args.network_allowed())
            .await?;
        let version = version.trim_start_matches('^');
        self.add(package, version)?;

        config.terminal().status(
            "Added",
            format!("dependency `{id}` with version `{version}`"),
        )?;

        Ok(())
    }

    async fn resolve_version(
        &self,
        config: &Config,
        metadata: &ComponentMetadata,
        id: &PackageId,
        network_allowed: bool,
    ) -> Result<String> {
        let mut resolver = DependencyResolver::new(
            config.warg(),
            &metadata.section.registries,
            None,
            config.terminal(),
            network_allowed,
        )?;
        let dependency = Dependency::Package(RegistryPackage {
            id: Some(self.package.id.clone()),
            version: self
                .package
                .version
                .as_ref()
                .unwrap_or(&VersionReq::STAR)
                .clone(),
            registry: self.registry.clone(),
        });

        resolver.add_dependency(id, &dependency).await?;

        let dependencies = resolver.resolve().await?;
        assert_eq!(dependencies.len(), 1);

        match dependencies.values().next().expect("expected a resolution") {
            DependencyResolution::Registry(resolution) => Ok(self
                .package
                .version
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| resolution.version.to_string())),
            _ => unreachable!(),
        }
    }

    fn add(&self, pkg: &Package, version: &str) -> Result<()> {
        let manifest = fs::read_to_string(&pkg.manifest_path).with_context(|| {
            format!(
                "failed to read manifest file `{path}`",
                path = pkg.manifest_path
            )
        })?;

        let mut document: Document = manifest.parse().with_context(|| {
            format!(
                "failed to parse manifest file `{path}`",
                path = pkg.manifest_path
            )
        })?;

        let dependencies = &mut document["package"]["metadata"]["component"]["dependencies"]
            .as_table_mut()
            .with_context(|| {
                format!(
                    "failed to find component metadata in manifest file `{path}`",
                    path = pkg.manifest_path
                )
            })?;

        match self.id.as_ref() {
            Some(id) => {
                dependencies[id.as_ref()] = value(InlineTable::from_iter([
                    ("package", Value::from(self.package.id.to_string())),
                    ("version", Value::from(version)),
                ]));
            }
            _ => {
                dependencies[self.package.id.as_ref()] = value(version);
            }
        }

        if self.dry_run {
            println!("{document}");
        } else {
            fs::write(&pkg.manifest_path, document.to_string()).with_context(|| {
                format!(
                    "failed to write manifest file `{path}`",
                    path = pkg.manifest_path
                )
            })?;
        }

        Ok(())
    }

    fn validate(&self, metadata: &ComponentMetadata, id: &PackageId) -> Result<()> {
        if metadata.section.dependencies.contains_key(id) {
            bail!("cannot add dependency `{id}` as it conflicts with an existing dependency");
        }

        Ok(())
    }
}
