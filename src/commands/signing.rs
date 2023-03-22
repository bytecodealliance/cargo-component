use crate::{
    signing::{delete_signing_key, get_signing_key_entry, set_signing_key},
    Config,
};
use anyhow::{bail, Context, Result};
use clap::{ArgAction, Args, Subcommand};
use p256::ecdsa::SigningKey;
use rand_core::OsRng;
use std::io::{self, Write};
use termcolor::{Color, ColorSpec};
use warg_crypto::signing::PrivateKey;

/// Manage signing keys for publishing components to a registry.
#[derive(Args)]
pub struct SigningCommand {
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
    pub command: SigningSubcommand,
}

impl SigningCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing signing command");

        config.cargo_mut().configure(
            u32::from(self.verbose),
            self.quiet,
            self.color.as_deref(),
            true,
            true,
            true,
            &None,
            &[],
            &[],
        )?;

        match self.command {
            SigningSubcommand::NewKey(cmd) => cmd.exec(config).await,
            SigningSubcommand::SetKey(cmd) => cmd.exec(config).await,
            SigningSubcommand::DeleteKey(cmd) => cmd.exec(config).await,
        }
    }
}

/// The subcommand to execute.
#[derive(Subcommand)]
pub enum SigningSubcommand {
    /// Creates a new signing key for a registry in the local keyring.
    NewKey(NewSigningKeyCommand),
    /// Sets the signing key for a registry in the local keyring.
    SetKey(SetSigningKeyCommand),
    /// Deletes the signing key for a registry from the local keyring.
    DeleteKey(DeleteSigningKeyCommand),
}

/// Creates a new signing key for a registry in the local keyring.
#[derive(Args)]
pub struct NewSigningKeyCommand {
    /// The user name to use for the signing key.
    #[clap(long, short, value_name = "USER", default_value = "default")]
    pub user: String,
    /// The host name of the registry to create a signing key for.
    #[clap(value_name = "HOST")]
    pub host: String,
}

impl NewSigningKeyCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        let entry = get_signing_key_entry(&self.host, &self.user)?;

        match entry.get_password() {
            Err(keyring::Error::NoEntry) => {
                // no entry exists, so we can continue
            }
            Ok(_) | Err(keyring::Error::Ambiguous(_)) => {
                bail!(
                    "a signing key already exists for user `{user}` of registry `{host}`",
                    user = self.user,
                    host = self.host
                );
            }
            Err(e) => {
                bail!(
                    "failed to get signing key for user `{user}` of registry `{host}`: {e}",
                    user = self.user,
                    host = self.host
                );
            }
        }

        let key = SigningKey::random(&mut OsRng).into();
        set_signing_key(&self.host, &self.user, &key)?;

        config.shell().note(format!(
            "created signing key for user `{user}` of registry `{host}`",
            user = self.user,
            host = self.host,
        ))?;

        Ok(())
    }
}

/// Sets the signing key for a registry in the local keyring.
#[derive(Args)]
pub struct SetSigningKeyCommand {
    /// The user name to use for the signing key.
    #[clap(long, short, value_name = "USER", default_value = "default")]
    pub user: String,
    /// The host name of the registry to set the signing key for.
    #[clap(value_name = "HOST")]
    pub host: String,
}

impl SetSigningKeyCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        let key: PrivateKey =
            rpassword::prompt_password("input signing key (expected format is `<alg>:<base64>`): ")
                .context("failed to read signing key")?
                .parse()
                .context("signing key is not in the correct format")?;

        set_signing_key(&self.host, &self.user, &key)?;

        config.shell().note(format!(
            "signing key for user `{user}` of registry `{host}` was set successfully",
            user = self.user,
            host = self.host,
        ))?;

        Ok(())
    }
}

/// Deletes the signing key for a registry from the local keyring.
#[derive(Args)]
pub struct DeleteSigningKeyCommand {
    /// The user name to use for the signing key.
    #[clap(long, short, value_name = "USER", default_value = "default")]
    pub user: String,
    /// The host name of the registry to delete the signing key for.
    #[clap(value_name = "HOST")]
    pub host: String,
}

impl DeleteSigningKeyCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        let mut yellow = ColorSpec::new();
        yellow.set_fg(Some(Color::Yellow));
        yellow.set_intense(true);

        config.shell().write_stdout(
            "⚠️  WARNING: this operation cannot be undone and the key will be permanently deleted ⚠️",
            &yellow,
        )?;

        config.shell().write_stdout(format!("\nare you sure you want to delete the signing key for user `{user}` of registry `{host}`? [type `yes` to confirm] ", user = self.user, host = self.host),
            &ColorSpec::new(),
        )?;

        io::stdout().flush().ok();

        let mut line = String::new();
        io::stdin().read_line(&mut line).ok();
        line.make_ascii_lowercase();

        if line.trim() != "yes" {
            config.shell().note(format!(
                "skipping deletion of signing key for registry `{host}`",
                host = self.host,
            ))?;
            return Ok(());
        }

        delete_signing_key(&self.host, &self.user)?;

        config.shell().note(format!(
            "signing key for user `{user}` of registry `{host}` was deleted successfully",
            user = self.user,
            host = self.host,
        ))?;

        Ok(())
    }
}
