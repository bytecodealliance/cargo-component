use anyhow::{Context, Result};
use cargo_component_core::{cache_dir, command::CommonOptions};
use clap::Args;
use wasm_pkg_client::caching::FileCache;

use crate::config::{Config, CONFIG_FILE_NAME};

/// Update dependencies as recorded in the lock file.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct UpdateCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Don't actually write the lockfile
    #[clap(long = "dry-run")]
    pub dry_run: bool,
}

impl UpdateCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing update command");

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

        crate::update_lockfile(
            &config,
            &config_path,
            pkg_config,
            &terminal,
            self.dry_run,
            file_cache,
        )
        .await
    }
}
