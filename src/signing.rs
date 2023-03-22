use anyhow::{bail, Context, Result};
use keyring::Entry;
use warg_crypto::signing::PrivateKey;

/// Gets the signing key entry for the given registry and user.
pub fn get_signing_key_entry(host: &str, user: &str) -> Result<Entry> {
    Entry::new(
        &format!("warg-signing-key:{host}", host = host.to_lowercase()),
        user,
    )
    .context("failed to get keyring entry")
}

/// Gets the signing key for the given registry host and user.
pub fn get_signing_key(host: &str, user: &str) -> Result<PrivateKey> {
    let entry = get_signing_key_entry(host, user)?;

    match entry.get_password() {
        Ok(secret) => secret.parse().context("failed to parse signing key"),
        Err(keyring::Error::NoEntry) => {
            bail!("no signing key found for user `{user}` of registry `{host}`");
        }
        Err(keyring::Error::Ambiguous(_)) => {
            bail!("more than one signing key found for user `{user}` of registry `{host}`");
        }
        Err(e) => {
            bail!("failed to get signing key for user `{user}` of registry `{host}`: {e}");
        }
    }
}

/// Sets the signing key for the given registry host and user.
pub fn set_signing_key(host: &str, user: &str, key: &PrivateKey) -> Result<()> {
    let entry = get_signing_key_entry(host, user)?;
    match entry.set_password(&key.to_string()) {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => {
            bail!("no signing key found for user `{user}` of registry `{host}`");
        }
        Err(keyring::Error::Ambiguous(_)) => {
            bail!("more than one signing key found for user `{user}` of registry `{host}`");
        }
        Err(e) => {
            bail!("failed to set signing key for user `{user}` of registry `{host}`: {e}");
        }
    }
}

pub fn delete_signing_key(host: &str, user: &str) -> Result<()> {
    let entry = get_signing_key_entry(host, user)?;
    match entry.delete_password() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => {
            bail!("no signing key found for user `{user}` of registry `{host}`");
        }
        Err(keyring::Error::Ambiguous(_)) => {
            bail!("more than one signing key found for user `{user}` of registry `{host}`");
        }
        Err(e) => {
            bail!("failed to set signing key for user `{user}` of registry `{host}`: {e}");
        }
    }
}
