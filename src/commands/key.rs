use crate::config::{CargoArguments, Config};
use anyhow::{bail, Context, Result};
use cargo_component_core::{
    keyring::{self, delete_signing_key, get_signing_key_entry, set_signing_key},
    terminal::Colors,
};
use clap::{ArgAction, Args, Subcommand};
use p256::ecdsa::SigningKey;
use rand_core::OsRng;
use std::io::{self};
use warg_crypto::signing::PrivateKey;

/// Manage signing keys for publishing components to a registry.
#[derive(Args)]
pub struct KeyCommand {
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

    /// The subcommand to execute.
    #[clap(subcommand)]
    pub command: KeySubcommand,
}

impl KeyCommand {
    /// Executes the command.
    pub async fn exec(self, config: &Config, _cargo_args: &CargoArguments) -> Result<()> {
        log::debug!("executing key command");

        match self.command {
            KeySubcommand::New(cmd) => cmd.exec(config).await,
            KeySubcommand::Set(cmd) => cmd.exec(config).await,
            KeySubcommand::Delete(cmd) => cmd.exec(config).await,
        }
    }
}

/// The subcommand to execute.
#[derive(Subcommand)]
pub enum KeySubcommand {
    /// Creates a new signing key for a registry in the local keyring.
    New(KeyNewCommand),
    /// Sets the signing key for a registry in the local keyring.
    Set(KeySetCommand),
    /// Deletes the signing key for a registry from the local keyring.
    Delete(KeyDeleteCommand),
}

/// Creates a new signing key for a registry in the local keyring.
#[derive(Args)]
pub struct KeyNewCommand {
    /// The key name to use for the signing key.
    #[clap(long, short, value_name = "NAME", default_value = "default")]
    pub key_name: String,
    /// The host name of the registry to create a signing key for.
    #[clap(value_name = "HOST")]
    pub host: String,
}

impl KeyNewCommand {
    /// Executes the command.
    pub async fn exec(self, config: &Config) -> Result<()> {
        let entry = get_signing_key_entry(&self.host, &self.key_name)?;

        match entry.get_password() {
            Err(keyring::Error::NoEntry) => {
                // no entry exists, so we can continue
            }
            Ok(_) | Err(keyring::Error::Ambiguous(_)) => {
                bail!(
                    "signing key `{name}` already exists for registry `{host}`",
                    name = self.key_name,
                    host = self.host
                );
            }
            Err(e) => {
                bail!(
                    "failed to get signing key `{name}` for registry `{host}`: {e}",
                    name = self.key_name,
                    host = self.host
                );
            }
        }

        let key = SigningKey::random(&mut OsRng).into();
        set_signing_key(&self.host, &self.key_name, &key)?;

        config.terminal().status(
            "Created",
            format!(
                "signing key `{name}` for registry `{host}`",
                name = self.key_name,
                host = self.host,
            ),
        )?;

        Ok(())
    }
}

/// Sets the signing key for a registry in the local keyring.
#[derive(Args)]
pub struct KeySetCommand {
    /// The key name to use for the signing key.
    #[clap(long, short, value_name = "NAME", default_value = "default")]
    pub key_name: String,
    /// The host name of the registry to set the signing key for.
    #[clap(value_name = "HOST")]
    pub host: String,
}

impl KeySetCommand {
    /// Executes the command.
    pub async fn exec(self, config: &Config) -> Result<()> {
        let key = PrivateKey::decode(
            rpassword::prompt_password("input signing key (expected format is `<alg>:<base64>`): ")
                .context("failed to read signing key")?,
        )
        .context("signing key is not in the correct format")?;

        set_signing_key(&self.host, &self.key_name, &key)?;

        config.terminal().status(
            "Set",
            format!(
                "signing key `{name}` for registry `{host}`",
                name = self.key_name,
                host = self.host,
            ),
        )?;

        Ok(())
    }
}

/// Deletes the signing key for a registry from the local keyring.
#[derive(Args)]
pub struct KeyDeleteCommand {
    /// The key name to use for the signing key.
    #[clap(long, short, value_name = "NAME", default_value = "default")]
    pub key_name: String,
    /// The host name of the registry to delete the signing key for.
    #[clap(value_name = "HOST")]
    pub host: String,
}

impl KeyDeleteCommand {
    /// Executes the command.
    pub async fn exec(self, config: &Config) -> Result<()> {
        config.terminal().write_stdout(
            "⚠️  WARNING: this operation cannot be undone and the key will be permanently deleted ⚠️",
            Some(Colors::Yellow),
        )?;

        config.terminal().write_stdout(
            format!("\nare you sure you want to delete signing key `{name}` for registry `{host}`? [type `yes` to confirm] ", name = self.key_name, host = self.host),
            None,
        )?;

        let mut line = String::new();
        io::stdin().read_line(&mut line).ok();
        line.make_ascii_lowercase();

        if line.trim() != "yes" {
            config.terminal().note(format!(
                "skipping deletion of signing key for registry `{host}`",
                host = self.host,
            ))?;
            return Ok(());
        }

        delete_signing_key(&self.host, &self.key_name)?;

        config.terminal().status(
            "Deleted",
            format!(
                "signing key `{name}` for registry `{host}`",
                name = self.key_name,
                host = self.host,
            ),
        )?;

        Ok(())
    }
}
