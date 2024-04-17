use crate::{
    config::{Config, CONFIG_FILE_NAME},
    CommandError,
};
use anyhow::{Context, Result};
use cargo_component_core::command::CommonOptions;
use clap::Args;
use warg_client::Retry;

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
    pub async fn exec(self, retry: Option<Retry>) -> Result<(), CommandError> {
        log::debug!("executing update command");

        let (config, config_path) = Config::from_default_file()?
            .with_context(|| format!("failed to find configuration file `{CONFIG_FILE_NAME}`"))?;

        let warg_config = warg_client::Config::from_default_file()?.unwrap_or_default();

        let terminal = self.common.new_terminal();
        crate::update_lockfile(
            &config,
            &config_path,
            &warg_config,
            &terminal,
            self.dry_run,
            retry,
        )
        .await
        .map_err(|e| e.into())
    }
}
