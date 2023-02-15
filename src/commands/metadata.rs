use crate::{commands::workspace, metadata, Config};
use anyhow::Result;
use cargo::{core::resolver::CliFeatures, ops::OutputMetadataOptions};
use clap::{value_parser, ArgAction, Args};
use std::path::PathBuf;

/// Output the resolved dependencies of a package, the concrete used versions
/// including overrides, in machine-readable format
#[derive(Args)]
pub struct MetadataCommand {
    /// Do not print cargo log messages
    #[clap(long = "quiet", short = 'q')]
    pub quiet: bool,

    /// Space or comma separated list of features to activate
    #[clap(long = "features", value_name = "FEATURES")]
    pub features: Vec<String>,

    /// Activate all available features
    #[clap(long = "all-features")]
    pub all_features: bool,

    /// Do not activate the `default` feature
    #[clap(long = "no-default-features")]
    pub no_default_features: bool,

    /// Only include resolve dependencies matching the given target triple
    #[clap(long = "filter-platform")]
    pub filter_platforms: Vec<String>,

    /// Use verbose output (-vv very verbose/build.rs output)
    #[clap(
        long = "verbose",
        short = 'v',
        action = ArgAction::Count
    )]
    pub verbose: u8,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

    /// Output information only about the workspace members and don't fetch dependencies
    #[clap(long = "no-deps")]
    pub no_deps: bool,

    /// Require Cargo.lock and cache are up to date
    #[clap(long = "frozen")]
    pub frozen: bool,

    /// Path to Cargo.toml
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Format version
    #[clap(
        long = "format-version",
        value_name = "VERSION",
        value_parser = value_parser!(u32).range(1..=1)
    )]
    pub format_version: Option<u32>,

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

impl MetadataCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing metadata command");

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

        let workspace = workspace(None, config)?;

        let version: u32 = match self.format_version {
            Some(version) => version,
            None => {
                config.shell().warn(
                    "please specify `--format-version` flag explicitly to avoid compatibility problems",
                )?;
                1
            }
        };

        let options = OutputMetadataOptions {
            cli_features: CliFeatures::from_command_line(
                &self.features,
                self.all_features,
                !self.no_default_features,
            )?,
            no_deps: self.no_deps,
            filter_platforms: self.filter_platforms,
            version,
        };

        let metadata = metadata(config, workspace, &options).await?;

        config.shell().print_json(&metadata)?;

        Ok(())
    }
}
