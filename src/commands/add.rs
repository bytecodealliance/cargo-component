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
    VersionedPackageName,
};
use cargo_metadata::Package;
use clap::Args;
use semver::VersionReq;
use std::{
    fs,
    path::{Path, PathBuf},
};
use toml_edit::{value, Document, InlineTable, Item, Table, Value};
use warg_protocol::registry::PackageName;

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

    /// The name of the dependency to use; defaults to the package name.
    #[clap(long, value_name = "NAME")]
    pub name: Option<PackageName>,

    /// The name of the package to add a dependency to.
    #[clap(value_name = "PACKAGE")]
    pub package: VersionedPackageName,

    /// Add the dependency to the list of target dependencies
    #[clap(long = "target")]
    pub target: bool,

    /// Add a package dependency to a file or directory.
    #[clap(long = "path", value_name = "PATH")]
    pub path: Option<PathBuf>,
}

impl AddCommand {
    /// Executes the command
    pub async fn exec(self) -> Result<()> {
        let config = Config::new(self.common.new_terminal())?;
        let metadata = load_metadata(self.manifest_path.as_deref())?;

        let spec = match &self.spec {
            Some(spec) => Some(spec.clone()),
            None => CargoPackageSpec::find_current_package_spec(&metadata),
        };

        let PackageComponentMetadata { package, metadata }: PackageComponentMetadata<'_> =
            match &spec {
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

        let name = match &self.name {
            Some(name) => name,
            None => &self.package.name,
        };

        self.validate(&metadata, name)?;

        if let Some(path) = self.path.as_ref() {
            self.add_from_path(package, path)?;

            config.terminal().status(
                "Added",
                format!(
                    "dependency `{name}` from path `{path}`",
                    path = path.to_str().unwrap()
                ),
            )?;
        } else {
            let version = self.resolve_version(&config, &metadata, name, true).await?;
            let version = version.trim_start_matches('^');
            self.add(package, version)?;

            config.terminal().status(
                "Added",
                format!("dependency `{name}` with version `{version}`"),
            )?;
        }

        Ok(())
    }

    async fn resolve_version(
        &self,
        config: &Config,
        metadata: &ComponentMetadata,
        name: &PackageName,
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
            name: Some(self.package.name.clone()),
            version: self
                .package
                .version
                .as_ref()
                .unwrap_or(&VersionReq::STAR)
                .clone(),
            registry: self.registry.clone(),
        });

        resolver.add_dependency(name, &dependency).await?;

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

    fn with_dependencies<F>(&self, pkg: &Package, body: F) -> Result<()>
    where
        F: FnOnce(&mut Table) -> Result<()>,
    {
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

        let metadata = document["package"]["metadata"]
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .context("section `package.metadata` is not a table")?;

        metadata.set_implicit(true);

        let component = metadata["component"]
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .context("section `package.metadata.component` is not a table")?;

        component.set_implicit(true);

        let dependencies = if self.target {
            let target = component["target"]
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .context("section `package.metadata.component.target` is not a table")?;

            target.set_implicit(true);

            target["dependencies"]
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .context(
                    "section `package.metadata.component.target.dependencies` is not a table",
                )?
        } else {
            component["dependencies"]
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .context("section `package.metadata.component.dependencies` is not a table")?
        };

        body(dependencies)?;

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

    fn add(&self, pkg: &Package, version: &str) -> Result<()> {
        self.with_dependencies(pkg, |dependencies| {
            match self.name.as_ref() {
                Some(name) => {
                    dependencies[name.as_ref()] = value(InlineTable::from_iter([
                        ("package", Value::from(self.package.name.to_string())),
                        ("version", Value::from(version)),
                    ]));
                }
                _ => {
                    dependencies[self.package.name.as_ref()] = value(version);
                }
            }
            Ok(())
        })
    }

    fn add_from_path(&self, pkg: &Package, path: &Path) -> Result<()> {
        self.with_dependencies(pkg, |dependencies| {
            let key = match self.name.as_ref() {
                Some(name) => name.as_ref(),
                None => self.package.name.as_ref(),
            };

            dependencies[key] = value(InlineTable::from_iter([(
                "path",
                Value::from(path.to_str().unwrap()),
            )]));

            Ok(())
        })
    }

    fn validate(&self, metadata: &ComponentMetadata, name: &PackageName) -> Result<()> {
        if self.target {
            match &metadata.section.target {
                Target::Package { .. } => {
                    bail!("cannot add dependency `{name}` to a registry package target")
                }
                Target::Local { dependencies, .. } => {
                    if dependencies.contains_key(name) {
                        bail!("cannot add dependency `{name}` as it conflicts with an existing dependency");
                    }
                }
            }
        } else if metadata.section.dependencies.contains_key(name) {
            bail!("cannot add dependency `{name}` as it conflicts with an existing dependency");
        }

        Ok(())
    }
}
