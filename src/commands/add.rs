use super::workspace;
use crate::{
    metadata::{ComponentMetadata, Dependency, Id, RegistryPackage},
    registry::{DependencyResolution, DependencyResolver},
    Config,
};
use anyhow::{bail, Context, Result};
use cargo::{core::package::Package, ops::Packages};
use clap::{ArgAction, Args};
use semver::VersionReq;
use std::{borrow::Cow, fs, path::PathBuf};
use toml_edit::{value, Document, InlineTable, Value};

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
    pub cargo_package: Option<String>,

    /// The name of the registry to use.
    #[clap(long = "registry", short = 'r', value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// The version requirement of the dependency being added.
    #[clap(long = "version", value_name = "VERSION")]
    pub version: Option<VersionReq>,

    /// The id of the dependency to use; defaults to the package name.
    #[clap(long, value_name = "ID")]
    pub id: Option<Id>,

    /// The name of the package to add a dependency to.
    #[clap(value_name = "PACKAGE")]
    pub package: String,
}

impl AddCommand {
    /// Executes the command
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        config.cargo_mut().configure(
            u32::from(self.verbose),
            self.quiet,
            self.color.as_deref(),
            false,
            false,
            false,
            &None,
            &[],
            &[],
        )?;

        let ws = workspace(self.manifest_path.as_deref(), config)?;
        let package = if let Some(ref inner) = self.cargo_package {
            let pkg = Packages::from_flags(false, vec![], vec![inner.clone()])?;
            pkg.get_packages(&ws)?[0]
        } else {
            ws.current()?
        };

        let metadata = match ComponentMetadata::from_package(package)? {
            Some(metadata) => metadata,
            None => bail!(
                "manifest `{path}` is not a WebAssembly component package",
                path = package.manifest_path().display(),
            ),
        };

        let id = match &self.id {
            Some(id) => Cow::Borrowed(id),
            None => {
                let id = self.package.parse::<Id>().ok();
                Cow::Owned(id.with_context(|| {
                    format!(
                        "package `{package}` is not a valid component model identifier; use the `--id` option to specify the identifier",
                        package = self.package
                    )
                })?)
            }
        };

        self.validate(&metadata, &id)?;

        let version = self.resolve_version(config, &metadata, &id).await?;
        let version = version.trim_start_matches('^');
        self.add(package, version)?;

        config.shell().status(
            "Added",
            format!("dependency `{id}` with version `{version}`"),
        )?;

        Ok(())
    }

    async fn resolve_version(
        &self,
        config: &Config,
        metadata: &ComponentMetadata,
        id: &Id,
    ) -> Result<String> {
        let mut resolver = DependencyResolver::new(config, &metadata.section.registries, None);
        let dependency = Dependency::Package(RegistryPackage {
            name: Some(self.package.clone()),
            version: self.version.as_ref().unwrap_or(&VersionReq::STAR).clone(),
            registry: self.registry.clone(),
        });

        resolver.add_dependency(id, &dependency, false).await?;

        let (target, dependencies) = resolver.resolve().await?;
        assert!(target.is_empty());
        assert_eq!(dependencies.len(), 1);

        match dependencies.values().next().expect("expected a resolution") {
            DependencyResolution::Registry(resolution) => Ok(self
                .version
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| resolution.version.to_string())),
            _ => unreachable!(),
        }
    }

    fn add(&self, pkg: &Package, version: &str) -> Result<()> {
        let manifest_path = pkg.manifest_path();
        let manifest = fs::read_to_string(manifest_path).with_context(|| {
            format!(
                "failed to read manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        let mut document: Document = manifest.parse().with_context(|| {
            format!(
                "failed to parse manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        let dependencies = &mut document["package"]["metadata"]["component"]["dependencies"]
            .as_table_mut()
            .with_context(|| {
                format!(
                    "failed to find component metadata in manifest file `{path}`",
                    path = manifest_path.display()
                )
            })?;

        match self.id.as_ref() {
            Some(id) => {
                dependencies[&id.to_string()] = value(InlineTable::from_iter([
                    ("package", Value::from(self.package.to_string())),
                    ("version", Value::from(version)),
                ]));
            }
            _ => {
                dependencies[&self.package.to_string()] = value(version);
            }
        }

        if self.dry_run {
            println!("{document}");
        } else {
            fs::write(manifest_path, document.to_string()).with_context(|| {
                format!(
                    "failed to write manifest file `{path}`",
                    path = manifest_path.display()
                )
            })?;
        }

        Ok(())
    }

    fn validate(&self, metadata: &ComponentMetadata, id: &Id) -> Result<()> {
        if metadata.section.dependencies.contains_key(id) {
            bail!("cannot add dependency `{id}` as it conflicts with an existing dependency");
        }

        Ok(())
    }
}
