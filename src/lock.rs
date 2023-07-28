//! Module for the lock file implementation.

use crate::config::{CargoArguments, Config};
use anyhow::Result;
use cargo_component_core::{lock::FileLock, terminal::Colors};
use cargo_metadata::Metadata;

/// The name of the lock file.
pub const LOCK_FILE_NAME: &str = "Cargo-component.lock";

pub(crate) fn acquire_lock_file_ro(
    config: &Config,
    workspace: &Metadata,
) -> Result<Option<FileLock>> {
    let path = workspace.workspace_root.join(LOCK_FILE_NAME);
    if !path.exists() {
        return Ok(None);
    }

    log::info!("opening lock file `{path}`");
    match FileLock::try_open_ro(&path)? {
        Some(lock) => Ok(Some(lock)),
        None => {
            config.terminal().status_with_color(
                "Blocking",
                format!("on access to lock file `{path}`"),
                Colors::Cyan,
            )?;

            FileLock::open_ro(&path).map(Some)
        }
    }
}

pub(crate) fn acquire_lock_file_rw(
    config: &Config,
    args: &CargoArguments,
    workspace: &Metadata,
) -> Result<FileLock> {
    if !args.lock_update_allowed() {
        let flag = if args.locked { "--locked" } else { "--frozen" };
        anyhow::bail!(
            "the lock file {path} needs to be updated but {flag} was passed to prevent this\n\
            If you want to try to generate the lock file without accessing the network, \
            remove the {flag} flag and use --offline instead.",
            path = workspace.workspace_root.join(LOCK_FILE_NAME)
        );
    }

    let path = workspace.workspace_root.join(LOCK_FILE_NAME);
    log::info!("creating lock file `{path}`");
    match FileLock::try_open_rw(&path)? {
        Some(lock) => Ok(lock),
        None => {
            config.terminal().status_with_color(
                "Blocking",
                format!("on access to lock file `{path}`"),
                Colors::Cyan,
            )?;

            FileLock::open_rw(&path)
        }
    }
}
