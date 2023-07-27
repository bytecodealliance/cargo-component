use super::CommonOptions;
use crate::config::{Config, CONFIG_FILE_NAME};
use anyhow::{bail, Context, Result};
use cargo_component_core::{
    registry::{Dependency, DependencyResolution, DependencyResolver, RegistryPackage},
    terminal::{Color, Terminal},
    VersionedPackageId,
};
use clap::Args;
use semver::VersionReq;
use warg_protocol::registry::PackageId;

async fn resolve_version(
    config: &Config,
    warg_config: &warg_client::Config,
    package: &VersionedPackageId,
    registry: &Option<String>,
    terminal: &Terminal,
) -> Result<String> {
    let mut resolver =
        DependencyResolver::new(warg_config, &config.registries, None, terminal, true)?;
    let dependency = Dependency::Package(RegistryPackage {
        id: Some(package.id.clone()),
        version: package
            .version
            .as_ref()
            .unwrap_or(&VersionReq::STAR)
            .clone(),
        registry: registry.clone(),
    });

    resolver.add_dependency(&package.id, &dependency).await?;

    let dependencies = resolver.resolve().await?;
    assert_eq!(dependencies.len(), 1);

    match dependencies.values().next().expect("expected a resolution") {
        DependencyResolution::Registry(resolution) => Ok(package
            .version
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| resolution.version.to_string())),
        _ => unreachable!(),
    }
}

/// Adds a reference to a WIT package from a registry.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct AddCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<Color>,

    /// Don't actually write the configuration file.
    #[clap(long = "dry-run")]
    pub dry_run: bool,

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
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing add command");

        let (mut config, config_path) = Config::from_default_file()?
            .with_context(|| format!("failed to find configuration file `{CONFIG_FILE_NAME}`"))?;

        let warg_config = warg_client::Config::from_default_file()?.unwrap_or_default();

        let id = self.id.as_ref().unwrap_or(&self.package.id);
        if config.dependencies.contains_key(id) {
            bail!("cannot add dependency `{id}` as it conflicts with an existing dependency");
        }

        let terminal = self.common.new_terminal();
        let version = resolve_version(
            &config,
            &warg_config,
            &self.package,
            &self.registry,
            &terminal,
        )
        .await?;

        let package = match &self.id {
            Some(id) => RegistryPackage {
                id: Some(id.clone()),
                version: version.parse()?,
                registry: self.registry,
            },
            None => version.parse()?,
        };

        config
            .dependencies
            .insert(id.clone(), Dependency::Package(package));

        if !self.dry_run {
            config.write(config_path)?;
        }

        terminal.status(
            if self.dry_run { "Would add" } else { "Added" },
            format!(
                "dependency `{id}` with version `{version}`{dry_run}",
                dry_run = if self.dry_run { " (dry run)" } else { "" }
            ),
        )?;

        Ok(())
    }
}
