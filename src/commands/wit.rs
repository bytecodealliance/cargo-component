use crate::{
    commands::workspace, metadata::ComponentMetadata, registry, signing, Config, PublishWitOptions,
};
use anyhow::{anyhow, bail, Context, Result};
use cargo::ops::Packages;
use clap::{ArgAction, Args, Subcommand};
use semver::Version;
use std::path::PathBuf;
use url::Url;
use warg_crypto::signing::PrivateKey;

/// Manages the target WIT package.
#[derive(Args)]
pub struct WitCommand {
    /// The subcommand to execute.
    #[clap(subcommand)]
    pub command: WitSubcommand,
}

impl WitCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing wit command");

        match self.command {
            WitSubcommand::Publish(cmd) => cmd.exec(config).await,
        }
    }
}

/// The subcommand to execute.
#[derive(Subcommand)]
pub enum WitSubcommand {
    /// Publishes the target WIT package to a registry.
    Publish(WitPublishCommand),
}

/// Publishes the target WIT package to a registry.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct WitPublishCommand {
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

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

    /// Path to the manifest to publish the WIT package for.
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Require Cargo.lock and cache are up to date
    #[clap(long = "frozen")]
    pub frozen: bool,

    /// Directory for all generated artifacts
    #[clap(long = "target-dir", value_name = "DIRECTORY")]
    pub target_dir: Option<PathBuf>,

    /// Require Cargo.lock is up to date
    #[clap(long = "locked")]
    pub locked: bool,

    /// Run without accessing the network
    #[clap(long = "offline")]
    pub offline: bool,

    /// Unstable (nightly-only) flags to Cargo, see 'cargo -Z help' for details
    #[clap(long = "Z", value_name = "FLAG")]
    pub unstable_flags: Vec<String>,

    /// Don't actually publish the package.
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// Cargo package to publish the WIT package for (see `cargo help pkgid`)
    #[clap(long = "package", short = 'p', value_name = "SPEC")]
    pub cargo_package: Option<String>,

    /// The user name to use for the signing key.
    #[clap(long, short, value_name = "USER", default_value = "default")]
    pub user: String,

    /// The registry to publish to.
    #[clap(long = "registry", value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// Initialize a new package in the registry.
    #[clap(long = "init")]
    pub init: bool,

    /// Overwrite the version of the package being published.
    #[clap(long = "version", value_name = "VERSION")]
    pub version: Option<Version>,

    /// The name of the package being published.
    #[clap(value_name = "PACKAGE")]
    pub name: String,
}

impl WitPublishCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        config.cargo_mut().configure(
            u32::from(self.verbose),
            self.quiet,
            self.color.as_deref(),
            self.frozen,
            self.locked,
            self.offline,
            &self.target_dir,
            &self.unstable_flags,
            &[],
        )?;

        let ws = workspace(self.manifest_path.as_deref(), config)?;
        let package = if let Some(ref inner) = self.cargo_package {
            let pkg = Packages::from_flags(false, vec![], vec![inner.clone()])?;
            pkg.get_packages(&ws)?[0]
        } else {
            ws.current()?
        };

        let metadata = match ComponentMetadata::from_package(package)? {
            Some(metadata) => metadata,
            None => bail!(
                "manifest `{path}` is not a WebAssembly component package",
                path = package.manifest_path().display(),
            ),
        };

        let url = registry::find_url(
            config,
            self.registry.as_deref(),
            &metadata.section.registries,
        )?;

        let signing_key: PrivateKey = if let Ok(key) = std::env::var("CARGO_COMPONENT_PUBLISH_KEY")
        {
            key.parse().context("failed to parse signing key from `CARGO_COMPONENT_PUBLISH_KEY` environment variable")?
        } else {
            let url: Url = url
                .parse()
                .with_context(|| format!("failed to parse registry URL `{url}`"))?;

            signing::get_signing_key(
                url.host_str()
                    .ok_or_else(|| anyhow!("registry URL `{url}` has no host"))?,
                &self.user,
            )?
        };

        let options = PublishWitOptions {
            cargo_package: self.cargo_package.as_deref(),
            name: &self.name,
            version: self.version.as_ref().unwrap_or(&metadata.version),
            url,
            signing_key,
            init: self.init,
            dry_run: self.dry_run,
        };

        crate::publish_wit(config, ws, &options).await
    }
}
