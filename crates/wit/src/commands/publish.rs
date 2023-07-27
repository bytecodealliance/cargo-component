use super::CommonOptions;
use crate::{
    config::{Config, CONFIG_FILE_NAME},
    publish_wit_package, PublishOptions,
};
use anyhow::{Context, Result};
use cargo_component_core::{keyring::get_signing_key, registry::find_url};
use clap::Args;
use warg_client::RegistryUrl;
use warg_crypto::signing::PrivateKey;
use warg_protocol::registry::PackageId;

/// Publish a WIT package to a registry.
#[derive(Args)]
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

    /// The key name to use for the signing key.
    #[clap(long, short, value_name = "KEY", default_value = "default")]
    pub key_name: String,

    /// Override the package name to publish.
    #[clap(long, value_name = "NAME")]
    pub package: Option<PackageId>,
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
            warg_config.default_url.as_deref(),
        )?;

        let signing_key = if let Ok(key) = std::env::var("WIT_PUBLISH_KEY") {
            PrivateKey::decode(key).context(
                "failed to parse signing key from `WIT_PUBLISH_KEY` environment variable",
            )?
        } else {
            let url: RegistryUrl = url
                .parse()
                .with_context(|| format!("failed to parse registry URL `{url}`"))?;

            get_signing_key(&url, &self.key_name)?
        };

        publish_wit_package(
            PublishOptions {
                config: &config,
                config_path: &config_path,
                warg_config: &warg_config,
                url,
                signing_key: &signing_key,
                package: self.package.as_ref(),
                init: self.init,
                dry_run: self.dry_run,
            },
            &terminal,
        )
        .await
    }
}
