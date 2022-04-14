//! Commands for the `cargo-component` CLI.

use anyhow::{bail, Result};
use cargo::{core::Workspace, util::important_paths::find_root_manifest_for_wd, Config};
use cargo_util::paths::normalize_path;
use std::path::{Path, PathBuf};

mod build;
mod new;

pub use self::build::*;
pub use self::new::*;

fn root_manifest(manifest_path: Option<&Path>, config: &Config) -> Result<PathBuf> {
    match manifest_path {
        Some(path) => {
            let normalized_path = normalize_path(path);
            if !normalized_path.ends_with("Cargo.toml") {
                bail!("the manifest-path must be a path to a Cargo.toml file")
            }
            if !normalized_path.exists() {
                bail!("manifest path `{}` does not exist", path.display())
            }
            Ok(normalized_path)
        }
        None => find_root_manifest_for_wd(config.cwd()),
    }
}

fn workspace<'a>(manifest_path: Option<&Path>, config: &'a Config) -> Result<Workspace<'a>> {
    let root = root_manifest(manifest_path, config)?;
    let mut ws = Workspace::new(&root, config)?;
    if config.cli_unstable().avoid_dev_deps {
        ws.set_require_optional_deps(false);
    }
    Ok(ws)
}
