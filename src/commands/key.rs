use crate::config::Config;
use anyhow::{Context, Result};
use cargo_component_core::{command::CommonOptions, terminal::Colors};
use clap::{Args, Subcommand};
use indexmap::IndexSet;
use p256::ecdsa::SigningKey;
use rand_core::OsRng;
use std::io::{self, Write};
use warg_credentials::keyring as warg_keyring;
use warg_crypto::signing::PrivateKey;
use warg_keyring::{delete_signing_key, get_signing_key, set_signing_key};

/// Manage signing keys for publishing components to a registry.
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
        log::debug!("executing key command");

        let mut config = Config::new(self.common.new_terminal())?;

        match self.command {
            KeySubcommand::Id(cmd) => cmd.exec(&config).await,
            KeySubcommand::New(cmd) => cmd.exec(&mut config).await,
            KeySubcommand::Set(cmd) => cmd.exec(&mut config).await,
            KeySubcommand::Delete(cmd) => cmd.exec(&config).await,
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
    /// The URL of the registry to print the Key ID for.
    #[clap(value_name = "URL")]
    pub url: String,
}

impl KeyIdCommand {
    /// Executes the command.
    pub async fn exec(self, config: &Config) -> Result<()> {
        let key = if let Some(keys) = &config.warg.keys {
            get_signing_key(Some(&self.url), &keys, config.warg.home_url.as_deref())?
        } else {
            get_signing_key(
                Some(&self.url),
                &IndexSet::new(),
                config.warg.home_url.as_deref(),
            )?
        };
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
    /// The URL of the registry to create a signing key for.
    #[clap(value_name = "URL")]
    pub url: String,
}

impl KeyNewCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        let key = SigningKey::random(&mut OsRng).into();
        if let Some(keys) = &mut config.warg.keys {
            set_signing_key(Some(&self.url), &key, keys, config.warg.home_url.as_deref())?;
        } else {
            set_signing_key(
                Some(&self.url),
                &key,
                &mut IndexSet::new(),
                config.warg.home_url.as_deref(),
            )?;
        };

        config.terminal().status(
            "Created",
            format!(
                "signing key ({fingerprint}) for registry `{url}`",
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
    /// The URL of the registry to create a signing key for.
    #[clap(value_name = "URL")]
    pub url: String,
}

impl KeySetCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        let key = PrivateKey::decode(
            rpassword::prompt_password("input signing key (expected format is `<alg>:<base64>`): ")
                .context("failed to read signing key")?,
        )
        .context("signing key is not in the correct format")?;

        // let key = PrivateKey::decode(key).context("signing key is not in the correct format")?;

        if config.warg.keys.is_none() {
            config.warg.keys = Some(IndexSet::new());
        }
        set_signing_key(
            Some(&self.url),
            &key,
            config.warg.keys.as_mut().unwrap(),
            config.warg.home_url.as_deref(),
        )?;
        config
            .warg
            .write_to_file(&warg_client::Config::default_config_path()?)?;

        config.terminal().status(
            "Set",
            format!(
                "signing key ({fingerprint}) for registry `{url}`",
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
    /// The URL of the registry to create a signing key for.
    #[clap(value_name = "URL")]
    pub url: String,
}

impl KeyDeleteCommand {
    /// Executes the command.
    pub async fn exec(self, config: &Config) -> Result<()> {
        config.terminal().write_stdout(
            "⚠️  WARNING: this operation cannot be undone and the key will be permanently deleted ⚠️",
            Some(Colors::Yellow),
        )?;

        config.terminal().write_stdout(
            format!(
                "\nare you sure you want to delete signing key for registry `{url}`? [type `yes` to confirm] ",
                url = self.url
            ),
            None,
        )?;

        io::stdout().flush().ok();

        let mut line = String::new();
        io::stdin().read_line(&mut line).ok();
        line.make_ascii_lowercase();

        if line.trim() != "yes" {
            config.terminal().note(format!(
                "skipping deletion of signing key for registry `{url}`",
                url = self.url,
            ))?;
            return Ok(());
        }

        delete_signing_key(
            Some(&self.url),
            &config.warg.keys.as_ref().expect("Please set a default signing key by typing `warg key set <alg:base64>` or `warg key new"),
            config.warg.home_url.as_deref(),
        )?;

        config.terminal().status(
            "Deleted",
            format!("signing key for registry `{url}`", url = self.url,),
        )?;

        Ok(())
    }
}
