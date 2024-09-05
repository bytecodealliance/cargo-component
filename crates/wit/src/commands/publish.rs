use anyhow::{Context, Result};
use cargo_component_core::{cache_dir, command::CommonOptions};
use clap::Args;
use wasm_pkg_client::{caching::FileCache, PackageRef, Registry};

use crate::{
    config::{Config, CONFIG_FILE_NAME},
    publish_wit_package, PublishOptions,
};

/// Publish a WIT package to a registry.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct PublishCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Don't actually publish the package.
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// Use the specified registry name when publishing the package.
    #[clap(long = "registry", value_name = "REGISTRY")]
    pub registry: Option<Registry>,

    /// Override the package name to publish.
    #[clap(long, value_name = "NAME")]
    pub package: Option<PackageRef>,
}

impl PublishCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing publish command");

        let (config, config_path) = Config::from_default_file()?
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

        publish_wit_package(
            PublishOptions {
                config: &config,
                config_path: &config_path,
                pkg_config,
                cache: file_cache,
                registry: self.registry.as_ref(),
                package: self.package.as_ref(),
                dry_run: self.dry_run,
            },
            &terminal,
        )
        .await
    }
}
