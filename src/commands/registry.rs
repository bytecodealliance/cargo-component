use crate::{metadata::PackageId, registry::local::LocalRegistry, Config};
use anyhow::{bail, Result};
use clap::{ArgAction, Args, Subcommand};
use semver::Version;
use std::path::PathBuf;

/// Interact with a local file system component registry.
#[derive(Args)]
pub struct RegistryCommand {
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

    /// The registry command to execute.
    #[clap(subcommand)]
    pub command: RegistrySubCommand,
}

impl RegistryCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        config.cargo_mut().configure(
            u32::from(self.verbose),
            self.quiet,
            self.color.as_deref(),
            false,
            false,
            false,
            &None,
            &[],
            &[],
        )?;

        match self.command {
            RegistrySubCommand::New(command) => command.exec(config).await,
            RegistrySubCommand::Publish(command) => command.exec(config).await,
        }
    }
}

/// Represents the possible registry subcommands.
#[derive(Subcommand)]
pub enum RegistrySubCommand {
    /// Create a new local file system component registry.
    New(RegistryNewCommand),
    /// Publish a package to a local file system component registry.
    Publish(RegistryPublishCommand),
}

/// Create a new local file system component registry.
#[derive(Args)]
pub struct RegistryNewCommand {
    /// The path for the component registry.
    #[clap(value_name = "PATH")]
    pub path: PathBuf,
}

impl RegistryNewCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing registry new command");

        if self.path.exists() {
            bail!("path `{path}` already exists", path = self.path.display());
        }

        config.shell().status(
            "Creating",
            format!(
                "local component registry at `{path}`",
                path = self.path.display()
            ),
        )?;

        LocalRegistry::open(config, &self.path, false)?;

        Ok(())
    }
}

/// Publish a package to a local file system component registry.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct RegistryPublishCommand {
    /// The path to the local component registry.
    #[clap(long, short, value_name = "REGISTRY")]
    pub registry: PathBuf,

    /// The ID of the package to publish.
    #[clap(long, value_name = "ID")]
    pub id: PackageId,

    /// The version of the package to publish.
    #[clap(long, short, value_name = "VERSION")]
    pub version: Version,

    /// The path to the package content to publish.
    #[clap(value_name = "PATH")]
    pub path: PathBuf,
}

impl RegistryPublishCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing registry publish command");

        if !self.registry.is_dir() {
            RegistryNewCommand {
                path: self.registry.clone(),
            }
            .exec(config)
            .await?;
        }

        let registry = LocalRegistry::open(config, &self.registry, false)?;

        config.shell().status(
            "Publishing",
            format!(
                "version {version} of package `{id}`",
                version = self.version,
                id = self.id
            ),
        )?;

        registry.publish(&self.id, &self.version, &self.path)?;

        Ok(())
    }
}
