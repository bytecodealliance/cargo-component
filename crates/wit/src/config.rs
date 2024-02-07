//! Module for WIT package configuration.

use anyhow::{Context, Result};
use cargo_component_core::registry::Dependency;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use toml_edit::Item;
use url::Url;
use warg_protocol::registry::PackageName;

/// The default name of the configuration file.
pub const CONFIG_FILE_NAME: &str = "wit.toml";

fn find_config(cwd: &Path) -> Option<PathBuf> {
    let mut current = Some(cwd);

    while let Some(dir) = current {
        let config = dir.join(CONFIG_FILE_NAME);
        if config.is_file() {
            return Some(config);
        }

        current = dir.parent();
    }

    None
}

/// Used to construct a new WIT package configuration.
#[derive(Default)]
pub struct ConfigBuilder {
    version: Option<Version>,
    registries: HashMap<String, Url>,
}

impl ConfigBuilder {
    /// Creates a new configuration builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the version to use in the configuration.
    pub fn with_version(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    /// Adds a registry to the configuration.
    pub fn with_registry(mut self, name: impl Into<String>, url: Url) -> Self {
        self.registries.insert(name.into(), url);
        self
    }

    /// Builds the configuration.
    pub fn build(self) -> Config {
        Config {
            version: self.version.unwrap_or_else(|| Version::new(0, 1, 0)),
            dependencies: Default::default(),
            registries: self.registries,
            authors: Default::default(),
            categories: Default::default(),
            description: None,
            license: None,
            documentation: None,
            homepage: None,
            repository: None,
        }
    }
}

/// Represents a WIT package configuration.
#[derive(Serialize, Deserialize)]
pub struct Config {
    /// The current package version.
    pub version: Version,
    /// The package dependencies.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub dependencies: HashMap<PackageName, Dependency>,
    /// The registries to use for sourcing packages.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub registries: HashMap<String, Url>,
    /// The authors of the package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    /// The categories of the package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    /// The package description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The package license.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// The package documentation URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    /// The package homepage URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// The package repository URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

impl Config {
    /// Loads a WIT package configuration from a default file path.
    ///
    /// This will search for a configuration file in the current directory and
    /// all parent directories.
    ///
    /// Returns both the configuration file and the path it was located at.
    ///
    /// Returns `Ok(None)` if no configuration file was found.
    pub fn from_default_file() -> Result<Option<(Self, PathBuf)>> {
        if let Some(path) = find_config(&std::env::current_dir()?) {
            return Ok(Some((Self::from_file(&path)?, path)));
        }

        Ok(None)
    }

    /// Loads a WIT package configuration from the given file path.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).with_context(|| {
            format!(
                "failed to read configuration file `{path}`",
                path = path.display()
            )
        })?;

        toml_edit::de::from_str(&contents).with_context(|| {
            format!(
                "failed to parse configuration file `{path}`",
                path = path.display()
            )
        })
    }

    /// Writes the configuration to the given file path.
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();

        let mut contents = toml_edit::ser::to_document(self).with_context(|| {
            format!(
                "failed to serialize configuration file `{path}`",
                path = path.display()
            )
        })?;

        // If the dependencies or registries tables are inline, convert
        // to a table
        for name in ["dependencies", "registries"] {
            if let Some(table) = contents.get_mut(name).and_then(Item::as_inline_table_mut) {
                let table = std::mem::take(table);
                contents[name] = Item::Table(table.into_table());
            }
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create parent directory for `{path}`",
                    path = path.display()
                )
            })?;
        }

        fs::write(path, contents.to_string()).with_context(|| {
            format!(
                "failed to write configuration file `{path}`",
                path = path.display()
            )
        })?;

        Ok(())
    }
}
