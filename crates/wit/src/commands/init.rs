use super::CommonOptions;
use crate::config::{ConfigBuilder, CONFIG_FILE_NAME};
use anyhow::{bail, Result};
use cargo_component_core::registry::DEFAULT_REGISTRY_NAME;
use clap::Args;
use std::path::PathBuf;
use url::Url;

/// Initialize a new WIT package.
#[derive(Args)]
pub struct InitCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Use the specified default registry when generating the package.
    #[clap(long = "registry", value_name = "REGISTRY")]
    pub registry: Option<Url>,

    /// The path to initialize the package in.
    #[clap(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,
}

impl InitCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing init command");

        let path = self.path.join(CONFIG_FILE_NAME);
        if path.is_file() {
            bail!(
                "WIT package configuration file `{path}` already exists",
                path = path.display()
            );
        }

        let terminal = self.common.new_terminal();
        let mut builder = ConfigBuilder::new();

        if let Some(registry) = self.registry {
            builder = builder.with_registry(DEFAULT_REGISTRY_NAME, registry);
        }

        let config = builder.build();
        config.write(&path)?;

        terminal.status(
            "Created",
            format!("configuration file `{path}`", path = path.display()),
        )?;

        Ok(())
    }
}
