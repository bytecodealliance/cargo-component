//! Module for cargo-component configuration.

use anyhow::{anyhow, Result};
use cargo::core::Shell;
use home::env::Env;
use std::{
    cell::RefMut,
    path::{Path, PathBuf},
};

fn home_with_cwd_env(env: &dyn Env, cwd: &Path) -> Result<PathBuf> {
    match env.var_os("CARGO_COMPONENT_HOME").filter(|h| !h.is_empty()) {
        Some(home) => {
            let home = PathBuf::from(home);
            if home.is_absolute() {
                Ok(home)
            } else {
                Ok(cwd.join(&home))
            }
        }
        _ => env
            .home_dir()
            .map(|p| p.join(".cargo-component"))
            .ok_or_else(|| anyhow!("could not find cargo component home directory")),
    }
}

/// Configuration information for cargo-component.
///
/// This struct is used to configure the behavior of cargo-component.
///
/// It also wraps the configuration of cargo itself.
#[derive(Debug)]
pub struct Config {
    /// The location of the user's `cargo-component` OS-dependent home directory.
    home_path: PathBuf,
    /// The configuration of `cargo` itself.
    cargo: cargo::Config,
}

impl Config {
    /// Create a new `Config` from the environment.
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let home_path = home_with_cwd_env(&home::env::OS_ENV, &cwd)?;
        let cargo = cargo::Config::default()?;
        Ok(Self { home_path, cargo })
    }

    /// Gets the home path of `cargo-component`.
    pub fn home_path(&self) -> &Path {
        &self.home_path
    }

    /// Gets the configuration of `cargo`.
    pub fn cargo(&self) -> &cargo::Config {
        &self.cargo
    }

    /// Gets the mutable configuration of `cargo`.
    pub fn cargo_mut(&mut self) -> &mut cargo::Config {
        &mut self.cargo
    }

    /// Gets a reference to the shell, e.g., for writing error messages.
    pub fn shell(&self) -> RefMut<'_, Shell> {
        self.cargo.shell()
    }
}
