use super::CommonOptions;
use anyhow::{bail, Context, Result};
use cargo_component_core::{
    keyring::{self, delete_signing_key, get_signing_key, get_signing_key_entry, set_signing_key},
    terminal::{Colors, Terminal},
};
use clap::{Args, Subcommand};
use p256::ecdsa::SigningKey;
use rand_core::OsRng;
use std::io::{self, Write};
use warg_client::RegistryUrl;
use warg_crypto::signing::PrivateKey;

/// Manage signing keys for publishing packages to a registry.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct KeyCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// The subcommand to execute.
    #[clap(subcommand)]
    pub command: KeySubcommand,
}

impl KeyCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        let terminal = self.common.new_terminal();

        match self.command {
            KeySubcommand::Id(cmd) => cmd.exec().await,
            KeySubcommand::New(cmd) => cmd.exec(&terminal).await,
            KeySubcommand::Set(cmd) => cmd.exec(&terminal).await,
            KeySubcommand::Delete(cmd) => cmd.exec(&terminal).await,
        }
    }
}

/// The subcommand to execute.
#[derive(Subcommand)]
pub enum KeySubcommand {
    /// Print the Key ID of the signing key for a registry in the local keyring.
    Id(KeyIdCommand),
    /// Creates a new signing key for a registry in the local keyring.
    New(KeyNewCommand),
    /// Sets the signing key for a registry in the local keyring.
    Set(KeySetCommand),
    /// Deletes the signing key for a registry from the local keyring.
    Delete(KeyDeleteCommand),
}

/// Print the Key ID of the signing key for a registry in the local keyring.
#[derive(Args)]
pub struct KeyIdCommand {
    /// The key name of the signing key.
    #[clap(long, short, value_name = "NAME", default_value = "default")]
    pub key_name: String,
    /// The URL of the registry to print the Key ID for.
    #[clap(value_name = "URL")]
    pub url: RegistryUrl,
}

impl KeyIdCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        let key = get_signing_key(&self.url, &self.key_name)?;
        println!(
            "{fingerprint}",
            fingerprint = key.public_key().fingerprint()
        );
        Ok(())
    }
}

/// Creates a new signing key for a registry in the local keyring.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct KeyNewCommand {
    /// The key name to use for the signing key.
    #[clap(long, short, value_name = "NAME", default_value = "default")]
    pub key_name: String,
    /// The URL of the registry to create a signing key for.
    #[clap(value_name = "URL")]
    pub url: RegistryUrl,
}

impl KeyNewCommand {
    /// Executes the command.
    pub async fn exec(self, terminal: &Terminal) -> Result<()> {
        let entry = get_signing_key_entry(&self.url, &self.key_name)?;

        match entry.get_password() {
            Err(keyring::Error::NoEntry) => {
                // no entry exists, so we can continue
            }
            Ok(_) | Err(keyring::Error::Ambiguous(_)) => {
                bail!(
                    "signing key `{name}` already exists for registry `{url}`",
                    name = self.key_name,
                    url = self.url
                );
            }
            Err(e) => {
                bail!(
                    "failed to get signing key `{name}` for registry `{url}`: {e}",
                    name = self.key_name,
                    url = self.url
                );
            }
        }

        let key = SigningKey::random(&mut OsRng).into();
        set_signing_key(&self.url, &self.key_name, &key)?;

        terminal.status(
            "Created",
            format!(
                "signing key `{name}` ({fingerprint}) for registry `{url}`",
                name = self.key_name,
                fingerprint = key.public_key().fingerprint(),
                url = self.url,
            ),
        )?;

        Ok(())
    }
}

/// Sets the signing key for a registry in the local keyring.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct KeySetCommand {
    /// The key name to use for the signing key.
    #[clap(long, short, value_name = "NAME", default_value = "default")]
    pub key_name: String,
    /// The URL of the registry to create a signing key for.
    #[clap(value_name = "URL")]
    pub url: RegistryUrl,
}

impl KeySetCommand {
    /// Executes the command.
    pub async fn exec(self, terminal: &Terminal) -> Result<()> {
        let key = PrivateKey::decode(
            rpassword::prompt_password("input signing key (expected format is `<alg>:<base64>`): ")
                .context("failed to read signing key")?,
        )
        .context("signing key is not in the correct format")?;

        set_signing_key(&self.url, &self.key_name, &key)?;

        terminal.status(
            "Set",
            format!(
                "signing key `{name}` ({fingerprint}) for registry `{url}`",
                name = self.key_name,
                fingerprint = key.public_key().fingerprint(),
                url = self.url,
            ),
        )?;

        Ok(())
    }
}

/// Deletes the signing key for a registry from the local keyring.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct KeyDeleteCommand {
    /// The key name to use for the signing key.
    #[clap(long, short, value_name = "NAME", default_value = "default")]
    pub key_name: String,
    /// The URL of the registry to create a signing key for.
    #[clap(value_name = "URL")]
    pub url: RegistryUrl,
}

impl KeyDeleteCommand {
    /// Executes the command.
    pub async fn exec(self, terminal: &Terminal) -> Result<()> {
        terminal.write_stdout(
            "⚠️  WARNING: this operation cannot be undone and the key will be permanently deleted ⚠️",
            Some(Colors::Yellow),
        )?;

        terminal.write_stdout(
            format!(
                "\nare you sure you want to delete signing key `{name}` for registry `{url}`? [type `yes` to confirm] ",
                name = self.key_name,
                url = self.url
            ),
            None,
        )?;

        io::stdout().flush().ok();

        let mut line = String::new();
        io::stdin().read_line(&mut line).ok();
        line.make_ascii_lowercase();

        if line.trim() != "yes" {
            terminal.note(format!(
                "skipping deletion of signing key for registry `{url}`",
                url = self.url,
            ))?;
            return Ok(());
        }

        delete_signing_key(&self.url, &self.key_name)?;

        terminal.status(
            "Deleted",
            format!(
                "signing key `{name}` for registry `{url}`",
                name = self.key_name,
                url = self.url,
            ),
        )?;

        Ok(())
    }
}
