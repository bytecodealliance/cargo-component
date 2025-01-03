use anyhow::Result;
use cargo_component_core::command::CommonOptions;
use clap::Args;

use crate::{
    config::Config, generate_bindings, load_component_metadata, load_metadata, CargoArguments,
};

/// Just update the generated bindings.
///
/// The generated bindings are generated automatically by subcommands like
/// `cargo component build`; `cargo component bindings` is for when one wishes
/// to just generate the bindings without doing any other work.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct BindingsCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,
}

impl BindingsCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("generating bindings");

        let config = Config::new(self.common.new_terminal(), self.common.config.clone()).await?;

        let client = config.client(self.common.cache_dir.clone(), false).await?;

        let cargo_args = CargoArguments::parse()?;
        let metadata = load_metadata(None)?;
        let packages =
            load_component_metadata(&metadata, cargo_args.packages.iter(), cargo_args.workspace)?;
        let _import_name_map =
            generate_bindings(client, &config, &metadata, &packages, &cargo_args).await?;

        Ok(())
    }
}
