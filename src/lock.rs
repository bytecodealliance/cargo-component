//! Module for the lock file implementation.

use anyhow::Result;
use cargo_component_core::{
    lock::FileLock,
    terminal::{Colors, Terminal},
};
use cargo_metadata::Metadata;

/// The name of the lock file.
pub const LOCK_FILE_NAME: &str = "Cargo-component.lock";

pub(crate) fn acquire_lock_file_ro(
    terminal: &Terminal,
    metadata: &Metadata,
) -> Result<Option<FileLock>> {
    let path = metadata.workspace_root.join(LOCK_FILE_NAME);
    if !path.exists() {
        return Ok(None);
    }

    log::info!("opening lock file `{path}`");
    match FileLock::try_open_ro(&path)? {
        Some(lock) => Ok(Some(lock)),
        None => {
            terminal.status_with_color(
                "Blocking",
                format!("on access to lock file `{path}`"),
                Colors::Cyan,
            )?;

            FileLock::open_ro(&path).map(Some)
        }
    }
}

pub(crate) fn acquire_lock_file_rw(
    terminal: &Terminal,
    metadata: &Metadata,
    lock_update_allowed: bool,
    locked: bool,
) -> Result<FileLock> {
    if !lock_update_allowed {
        let flag = if locked { "--locked" } else { "--frozen" };
        anyhow::bail!(
            "the lock file {path} needs to be updated but {flag} was passed to prevent this\n\
            If you want to try to generate the lock file without accessing the network, \
            remove the {flag} flag and use --offline instead.",
            path = metadata.workspace_root.join(LOCK_FILE_NAME)
        );
    }

    let path = metadata.workspace_root.join(LOCK_FILE_NAME);
    log::info!("creating lock file `{path}`");
    match FileLock::try_open_rw(&path)? {
        Some(lock) => Ok(lock),
        None => {
            terminal.status_with_color(
                "Blocking",
                format!("on access to lock file `{path}`"),
                Colors::Cyan,
            )?;

            FileLock::open_rw(&path)
        }
    }
}
