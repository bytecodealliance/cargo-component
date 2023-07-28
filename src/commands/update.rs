use crate::{config::CargoArguments, load_component_metadata, load_metadata, Config};
use anyhow::Result;
use clap::{ArgAction, Args};
use std::path::PathBuf;

/// Update dependencies as recorded in the component lock file
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct UpdateCommand {
    /// Do not print cargo log messages
    #[clap(long = "quiet", short = 'q')]
    pub quiet: bool,

    /// Use verbose output (-vv very verbose/build.rs output)
    #[clap(
        long = "verbose",
        short = 'v',
        action = ArgAction::Count
    )]
    pub verbose: u8,

    /// Don't actually write the lockfile
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

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
    pub async fn exec(self, config: &Config, cargo_args: &CargoArguments) -> Result<()> {
        log::debug!("executing update command");
        let metadata = load_metadata(cargo_args.manifest_path.as_deref())?;
        let packages = load_component_metadata(&metadata, [].iter(), true)?;
        crate::update_lockfile(config, &metadata, &packages, cargo_args, self.dry_run).await
    }
}
