use crate::{load_component_metadata, load_metadata, Config};
use anyhow::Result;
use cargo_component_core::command::CommonOptions;
use clap::Args;
use std::path::PathBuf;

/// Update dependencies as recorded in the component lock file
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct UpdateCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Don't actually write the lockfile
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// Require lock file and cache are up to date
    #[clap(long = "frozen")]
    pub frozen: bool,

    /// Path to Cargo.toml
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Require lock file is up to date
    #[clap(long = "locked")]
    pub locked: bool,

    /// Run without accessing the network
    #[clap(long = "offline")]
    pub offline: bool,
}

impl UpdateCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing update command");
        let config = Config::new(self.common.new_terminal())?;
        let metadata = load_metadata(self.manifest_path.as_deref())?;
        let packages = load_component_metadata(&metadata, [].iter(), true)?;

        let network_allowed = !self.frozen && !self.offline;
        let lock_update_allowed = !self.frozen && !self.locked;
        crate::update_lockfile(
            &config,
            &metadata,
            &packages,
            network_allowed,
            lock_update_allowed,
            self.locked,
            self.dry_run,
        )
        .await
    }
}
