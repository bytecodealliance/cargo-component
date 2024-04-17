use crate::config::{Config, CONFIG_FILE_NAME};
use anyhow::{anyhow, Context, Result};
use cargo_component_core::{
    command::CommonOptions,
    registry::{
        CommandError, Dependency, DependencyResolution, DependencyResolver, RegistryPackage,
    },
    terminal::Terminal,
    VersionedPackageName,
};
use clap::Args;
use semver::VersionReq;
use std::path::PathBuf;
use warg_client::Retry;
use warg_protocol::registry::PackageName;

async fn resolve_version(
    warg_config: &warg_client::Config,
    package: &VersionedPackageName,
    registry: &Option<String>,
    terminal: &Terminal,
    retry: Option<Retry>,
) -> Result<String, CommandError> {
    let mut resolver = DependencyResolver::new(warg_config, None, terminal, true)?;
    let dependency = Dependency::Package(RegistryPackage {
        name: Some(package.name.clone()),
        version: package
            .version
            .as_ref()
            .unwrap_or(&VersionReq::STAR)
            .clone(),
        registry: registry.clone(),
    });

    resolver
        .add_dependency(&package.name, &dependency, retry.as_ref())
        .await?;

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

    /// Don't actually write the configuration file.
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// The name of the registry to use.
    #[clap(long = "registry", short = 'r', value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// The name of the dependency to use; defaults to the package name.
    #[clap(long, value_name = "NAME")]
    pub name: Option<PackageName>,

    /// Add a package dependency to a file or directory.
    #[clap(long = "path", value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// The id of the package to add a dependency to.
    #[clap(value_name = "PACKAGE")]
    pub package: VersionedPackageName,
}

impl AddCommand {
    /// Executes the command.
    pub async fn exec(self, retry: Option<Retry>) -> Result<(), CommandError> {
        log::debug!("executing add command");

        let (mut config, config_path) = Config::from_default_file()?
            .with_context(|| format!("failed to find configuration file `{CONFIG_FILE_NAME}`"))?;

        let name = self.name.as_ref().unwrap_or(&self.package.name);
        if config.dependencies.contains_key(name) {
            return Err(anyhow!(
                "cannot add dependency `{name}` as it conflicts with an existing dependency"
            )
            .into());
        }

        let warg_config = warg_client::Config::from_default_file()?.unwrap_or_default();
        let terminal = self.common.new_terminal();
        let message = match self.path.as_deref() {
            Some(path) => {
                config
                    .dependencies
                    .insert(name.clone(), Dependency::Local(path.to_path_buf()));

                format!(
                    "dependency `{name}` from path `{path}`{dry_run}",
                    path = path.display(),
                    dry_run = if self.dry_run { " (dry run)" } else { "" }
                )
            }
            None => {
                let version = resolve_version(
                    &warg_config,
                    &self.package,
                    &self.registry,
                    &terminal,
                    retry,
                )
                .await?;

                let package = RegistryPackage {
                    name: self.name.is_some().then(|| self.package.name.clone()),
                    version: version.parse().expect("expected a valid version"),
                    registry: self.registry,
                };

                config
                    .dependencies
                    .insert(name.clone(), Dependency::Package(package));

                format!(
                    "dependency `{name}` with version `{version}`{dry_run}",
                    dry_run = if self.dry_run { " (dry run)" } else { "" }
                )
            }
        };

        if !self.dry_run {
            config.write(config_path)?;
        }

        terminal.status(if self.dry_run { "Would add" } else { "Added" }, message)?;

        Ok(())
    }
}
