use super::workspace;
use crate::{
    metadata::{ComponentMetadata, PackageId},
    registry::{self, DEFAULT_REGISTRY_NAME},
    Config,
};
use anyhow::{bail, Context, Result};
use cargo::{core::package::Package, ops::Packages};
use clap::{ArgAction, Args};
use semver::VersionReq;
use std::{fs, path::PathBuf};
use toml_edit::{value, Document};

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

    /// Name to use for the dependency.
    #[clap(long = "name", short = 'n', value_name = "NAME")]
    pub name: Option<String>,

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

    /// The package to add a dependency to.
    #[clap(value_name = "PACKAGE")]
    pub package: PackageId,
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

        self.validate(&metadata)?;
        let version = self.resolve_version(config, &metadata)?;
        let version = version.trim_start_matches('^');
        self.add(package, version)?;

        config.shell().status(
            "Added",
            format!(
                "dependency `{name}` with version `{version}`",
                name = self.name()
            ),
        )?;

        Ok(())
    }

    fn resolve_version(&self, config: &Config, metadata: &ComponentMetadata) -> Result<String> {
        let name = self.registry.as_deref().unwrap_or(DEFAULT_REGISTRY_NAME);
        let registry = registry::create(config, name, &metadata.section.registries)?;

        match registry.resolve(&self.package, self.version.as_ref())? {
            Some(r) => Ok(self
                .version
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| r.version.to_string())),
            None => match &self.version {
                Some(version) => bail!(
                    "package `{package}` has no release that satisfies version requirement `{version}`",
                    package = self.package,
                ),
                None => bail!("package `{package}` has not been released", package = self.package),
            },
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

        dependencies[self.name()] = value(format!("{pkg}:{version}", pkg = self.package,));

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

    fn validate(&self, metadata: &ComponentMetadata) -> Result<()> {
        let name = self.name();
        if metadata.name == name {
            bail!(
                "cannot add dependency `{name}` as it conflicts with the component's package name"
            );
        }

        if metadata.section.dependencies.contains_key(name) {
            bail!(
                "cannot add dependency `{name}` as it conflicts with an existing dependency",
                name = name
            );
        }

        Ok(())
    }

    fn name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.package.name)
    }
}
