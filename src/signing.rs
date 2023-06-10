use anyhow::{bail, Context, Result};
use keyring::Entry;
use warg_crypto::signing::PrivateKey;

/// Gets the signing key entry for the given registry and key name.
pub fn get_signing_key_entry(host: &str, name: &str) -> Result<Entry> {
    Entry::new(
        &format!("warg-signing-key:{host}", host = host.to_lowercase()),
        name,
    )
    .context("failed to get keyring entry")
}

/// Gets the signing key for the given registry host and key name.
pub fn get_signing_key(host: &str, name: &str) -> Result<PrivateKey> {
    let entry = get_signing_key_entry(host, name)?;

    match entry.get_password() {
        Ok(secret) => secret.parse().context("failed to parse signing key"),
        Err(keyring::Error::NoEntry) => {
            bail!("no signing key found with name `{name}` for registry `{host}`");
        }
        Err(keyring::Error::Ambiguous(_)) => {
            bail!("more than one signing key with name `{name}` for registry `{host}`");
        }
        Err(e) => {
            bail!("failed to get signing key with name `{name}` for registry `{host}`: {e}");
        }
    }
}

/// Sets the signing key for the given registry host and key name.
pub fn set_signing_key(host: &str, name: &str, key: &PrivateKey) -> Result<()> {
    let entry = get_signing_key_entry(host, name)?;
    match entry.set_password(&key.to_string()) {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => {
            bail!("no signing key found with name `{name}` for registry `{host}`");
        }
        Err(keyring::Error::Ambiguous(_)) => {
            bail!("more than one signing key found with name `{name}` for registry `{host}`");
        }
        Err(e) => {
            bail!("failed to set signing key with name `{name}` for registry `{host}`: {e}");
        }
    }
}

pub fn delete_signing_key(host: &str, name: &str) -> Result<()> {
    let entry = get_signing_key_entry(host, name)?;
    match entry.delete_password() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => {
            bail!("no signing key found with name `{name}` for registry `{host}`");
        }
        Err(keyring::Error::Ambiguous(_)) => {
            bail!("more than one signing key found with name `{name}` for registry `{host}`");
        }
        Err(e) => {
            bail!("failed to delete signing key with name `{name}` for registry `{host}`: {e}");
        }
    }
}
