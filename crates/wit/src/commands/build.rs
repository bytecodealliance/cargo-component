use crate::{
    build_wit_package,
    config::{Config, CONFIG_FILE_NAME},
};
use anyhow::{Context, Result};
use cargo_component_core::command::CommonOptions;
use clap::Args;
use std::{fs, path::PathBuf};

/// Build a binary WIT package.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct BuildCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// The output package path.
    #[clap(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,
}

impl BuildCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing build command");

        let (config, config_path) = Config::from_default_file()?
            .with_context(|| format!("failed to find configuration file `{CONFIG_FILE_NAME}`"))?;

        let warg_config = warg_client::Config::from_default_file()?.unwrap_or_default();

        let terminal = self.common.new_terminal();
        let (id, bytes) = build_wit_package(&config, &config_path, &warg_config, &terminal).await?;

        let output = self
            .output
            .unwrap_or_else(|| format!("{name}.wasm", name = id.name()).into());

        fs::write(&output, bytes).with_context(|| {
            format!(
                "failed to write output file `{output}`",
                output = output.display()
            )
        })?;

        terminal.status(
            "Created",
            format!("package `{output}`", output = output.display()),
        )?;

        Ok(())
    }
}
