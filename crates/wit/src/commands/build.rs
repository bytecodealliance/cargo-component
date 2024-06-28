use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use cargo_component_core::{cache_dir, command::CommonOptions};
use clap::Args;
use wasm_pkg_client::caching::FileCache;

use crate::{
    build_wit_package,
    config::{Config, CONFIG_FILE_NAME},
};

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

        let (id, bytes) =
            build_wit_package(&config, &config_path, pkg_config, &terminal, file_cache).await?;

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
