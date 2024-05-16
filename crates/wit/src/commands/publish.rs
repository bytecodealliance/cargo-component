use crate::{
    config::{Config, CONFIG_FILE_NAME},
    publish_wit_package, PublishOptions,
};
use anyhow::{Context, Result};
use cargo_component_core::{command::CommonOptions, registry::find_url};
use clap::Args;
use warg_crypto::signing::PrivateKey;
use warg_protocol::registry::PackageName;

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

    /// Initialize a new package in the registry.
    #[clap(long = "init")]
    pub init: bool,

    /// Use the specified registry name when publishing the package.
    #[clap(long = "registry", value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// Override the package name to publish.
    #[clap(long, value_name = "NAME")]
    pub package: Option<PackageName>,
}

impl PublishCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing publish command");

        let (config, config_path) = Config::from_default_file()?
            .with_context(|| format!("failed to find configuration file `{CONFIG_FILE_NAME}`"))?;

        let terminal = self.common.new_terminal();
        let warg_config = warg_client::Config::from_default_file()?.unwrap_or_default();

        let url = find_url(
            self.registry.as_deref(),
            &config.registries,
            warg_config.home_url.as_deref(),
        )?;

        let signing_key = if let Ok(key) = std::env::var("WIT_PUBLISH_KEY") {
            Some(PrivateKey::decode(key).context(
                "failed to parse signing key from `WIT_PUBLISH_KEY` environment variable",
            )?)
        } else {
            None
        };

        publish_wit_package(
            PublishOptions {
                config: &config,
                config_path: &config_path,
                warg_config: &warg_config,
                url,
                signing_key,
                package: self.package.as_ref(),
                init: self.init,
                dry_run: self.dry_run,
            },
            &terminal,
        )
        .await
    }
}
