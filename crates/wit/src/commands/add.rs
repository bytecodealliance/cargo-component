use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use cargo_component_core::{
    cache_dir,
    command::CommonOptions,
    registry::{Dependency, DependencyResolution, DependencyResolver, RegistryPackage},
    VersionedPackageName,
};
use clap::Args;
use semver::VersionReq;
use wasm_pkg_client::{caching::FileCache, PackageRef};

use crate::config::{Config, CONFIG_FILE_NAME};

async fn resolve_version(
    pkg_config: Option<wasm_pkg_client::Config>,
    package: &VersionedPackageName,
    registry: &Option<String>,
    file_cache: FileCache,
) -> Result<String> {
    let mut resolver = DependencyResolver::new(pkg_config, None, file_cache)?;
    let dependency = Dependency::Package(RegistryPackage {
        name: Some(package.name.clone()),
        version: package
            .version
            .as_ref()
            .unwrap_or(&VersionReq::STAR)
            .clone(),
        registry: registry.clone(),
    });

    resolver.add_dependency(&package.name, &dependency).await?;

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
    pub name: Option<PackageRef>,

    /// Add a package dependency to a file or directory.
    #[clap(long = "path", value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// The id of the package to add a dependency to.
    #[clap(value_name = "PACKAGE")]
    pub package: VersionedPackageName,
}

impl AddCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing add command");

        let (mut config, config_path) = Config::from_default_file()?
            .with_context(|| format!("failed to find configuration file `{CONFIG_FILE_NAME}`"))?;

        let terminal = self.common.new_terminal();
        let pkg_config = if let Some(config_file) = self.common.config {
            wasm_pkg_client::Config::from_file(&config_file).context(format!(
                "failed to load configuration file from {}",
                config_file.display()
            ))?
        } else {
            wasm_pkg_client::Config::global_defaults()?
        };

        let file_cache = FileCache::new(cache_dir(self.common.cache_dir)?).await?;

        let name = self.name.as_ref().unwrap_or(&self.package.name);
        if config.dependencies.contains_key(name) {
            bail!("cannot add dependency `{name}` as it conflicts with an existing dependency");
        }

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
                let version =
                    resolve_version(Some(pkg_config), &self.package, &self.registry, file_cache)
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
