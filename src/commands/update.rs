use crate::{commands::workspace, Config};
use anyhow::Result;
use cargo::ops::UpdateOptions;
use clap::{ArgAction, Args};
use std::path::PathBuf;

/// Update dependencies as recorded in the local lock files
#[derive(Args)]
pub struct UpdateCommand {
    /// Do not print cargo log messages
    #[clap(long = "quiet", short = 'q')]
    pub quiet: bool,

    /// Check all cargo packages in the workspace
    #[clap(long = "workspace", alias = "all")]
    pub workspace: bool,

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

    /// Require Cargo.lock and cache are up to date
    #[clap(long = "frozen")]
    pub frozen: bool,

    /// Path to Cargo.toml
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Require Cargo.lock is up to date
    #[clap(long = "locked")]
    pub locked: bool,

    /// Run without accessing the network
    #[clap(long = "offline")]
    pub offline: bool,

    /// Unstable (nightly-only) flags to Cargo, see 'cargo -Z help' for details
    #[clap(long = "Z", value_name = "FLAG")]
    pub unstable_flags: Vec<String>,
}

impl UpdateCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing update command");

        config.cargo_mut().configure(
            u32::from(self.verbose),
            self.quiet,
            self.color.as_deref(),
            self.frozen,
            self.locked,
            self.offline,
            &None,
            &self.unstable_flags,
            &[],
        )?;

        let options = UpdateOptions {
            aggressive: false,
            precise: None,
            to_update: Vec::default(),
            dry_run: self.dry_run,
            workspace: self.workspace,
            config: config.cargo(),
        };

        let workspace = workspace(self.manifest_path.as_deref(), config)?;
        crate::update_lockfile(config, &workspace, &options).await
    }
}
