use crate::{
    config::CargoPackageSpec,
    load_component_metadata, load_metadata,
    metadata::{ComponentMetadata, Target},
    Config, PackageComponentMetadata,
};
use anyhow::{bail, Context, Result};
use cargo_component_core::{
    command::CommonOptions,
    registry::{Dependency, DependencyResolution, DependencyResolver, RegistryPackage},
    VersionedPackageId,
};
use cargo_metadata::Package;
use clap::Args;
use semver::VersionReq;
use std::{fs, path::PathBuf};
use toml_edit::{value, Document, InlineTable, Item, Table, Value};
use warg_protocol::registry::PackageId;

/// Add a dependency for a WebAssembly component
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct AddCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

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

    /// Add the dependency to the list of target dependencies
    #[clap(long = "target")]
    pub target: bool,

    /// Add a package dependency to this directory.
    #[clap(long = "path", value_name = "PATH")]
    pub path: Option<PathBuf>,
}

impl AddCommand {
    /// Executes the command
    pub async fn exec(self) -> Result<()> {
        let config = Config::new(self.common.new_terminal())?;
        let metadata = load_metadata(self.manifest_path.as_deref())?;

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

        let version = self.resolve_version(&config, &metadata, id, true).await?;
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
        let dependency = match self.path.as_ref() {
            Some(path) => Dependency::Local(path.clone()),
            None => Dependency::Package(RegistryPackage {
                id: Some(self.package.id.clone()),
                version: self
                    .package
                    .version
                    .as_ref()
                    .unwrap_or(&VersionReq::STAR)
                    .clone(),
                registry: self.registry.clone(),
            }),
        };

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

            // There's no version information present for local dependencies, so we return "*"
            // here.
            DependencyResolution::Local(_) => Ok(String::from("*")),
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

        let dependencies = if self.target {
            document["package"]["metadata"]["component"]["target"]["dependencies"]
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .unwrap()
        } else {
            document["package"]["metadata"]["component"]["dependencies"]
                .as_table_mut()
                .with_context(|| {
                    format!(
                        "failed to find component metadata in manifest file `{path}`",
                        path = pkg.manifest_path
                    )
                })?
        };

        let mut config = InlineTable::new();

        if self.id.is_some() {
            config.insert("package", Value::from(self.package.id.to_string()));
        }

        if let Some(path) = self.path.as_ref() {
            config.insert("path", Value::from(path.to_str().unwrap()));
        }

        if config.is_empty() {
            dependencies[self.package.id.as_ref()] = value(version);
        } else {
            config.insert("version", Value::from(version));
            dependencies[self.package.id.as_ref()] = value(config);
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
        if self.target {
            match &metadata.section.target {
                Some(Target::Package { .. }) => {
                    bail!("cannot add dependency `{id}` to a registry package target")
                }
                Some(Target::Local { dependencies, .. }) => {
                    if dependencies.contains_key(id) {
                        bail!("cannot add dependency `{id}` as it conflicts with an existing dependency");
                    }
                }
                None => {}
            }
        } else {
            if metadata.section.dependencies.contains_key(id) {
                bail!("cannot add dependency `{id}` as it conflicts with an existing dependency");
            }
        }

        Ok(())
    }
}
