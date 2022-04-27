use super::workspace;
use crate::ComponentMetadata;
use anyhow::{bail, Context, Result};
use cargo::{core::package::Package, ops::Packages, Config};
use clap::Args;
use std::{fs, path::PathBuf};
use toml_edit::{table, value, Document, InlineTable, Value};

/// Add a depency for a WebAssembly component
#[derive(Args)]
pub struct AddCommand {
    /// Do not print cargo log messages
    #[clap(long = "quiet", short = 'q')]
    pub quiet: bool,
    ///
    /// Use verbose output (-vv very verbose/build.rs output)
    #[clap(
        long = "verbose",
        short = 'v',
        takes_value = false,
        parse(from_occurrences)
    )]
    pub verbose: u32,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

    /// Path to the interface definition of the dependency
    #[clap(long = "path", value_name = "PATH")]
    pub path: String,

    /// Set the version of the dependency
    #[clap(long = "version", value_name = "VERSION")]
    pub version: Option<String>,

    /// Name of the dependency
    #[clap(value_name = "name")]
    pub name: String,

    /// Set the dependency as an exported interface
    #[clap(long = "export")]
    pub export: bool,

    /// Path to the manifest to add a dependency to
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Don't actually write the manifest
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// Package to add the dependency to (see `cargo help pkgid`)
    #[clap(long = "package", short = 'p', value_name = "SPEC")]
    pub packages: Vec<String>,
}

impl AddCommand {
    /// Executes the command
    pub fn exec(self, config: &mut Config) -> Result<()> {
        config.configure(
            self.verbose,
            self.quiet,
            self.color.as_deref(),
            false,
            false,
            false,
            &None,
            &[],
            &[],
        )?;

        let ws = workspace(self.manifest_path.as_deref(), config)?;
        let package = if !self.packages.is_empty() {
            if self.packages.len() > 1 {
                bail!("only one package can be specified");
            }

            let pkg = Packages::from_flags(false, vec![], self.packages.clone())?;
            let packages = pkg.get_packages(&ws)?;

            if packages.len() != 1 {
                bail!("no packages found matching the specifier");
            }

            packages[0]
        } else {
            let package = ws.current()?;
            package
        };

        let component_metadata = ComponentMetadata::from_package(config, &package)?;

        self.validate(&component_metadata)
            .and_then(|_| self.add(&package))?;

        config
            .shell()
            .status("Added component dependency", format!("{}", self.name))?;

        Ok(())
    }

    fn add(&self, pkg: &Package) -> Result<()> {
        let manifest_path = pkg.manifest_path();
        let manifest = fs::read_to_string(&manifest_path).with_context(|| {
            format!("failed to read manifest file `{}`", manifest_path.display())
        })?;

        let mut document: Document = manifest.parse().with_context(|| {
            format!(
                "failed to parse manifest file `{}`",
                manifest_path.display()
            )
        })?;

        let component = &mut document["package"]["metadata"]["component"]
            .as_table_mut()
            .with_context(|| {
                format!(
                    "failed to find component metadata in manifest file `{}`",
                    manifest_path.display()
                )
            })?;

        let deps = component.entry("dependencies").or_insert(table());
        let mut inline_table = vec![("path", Value::from(self.path.clone()))];

        if let Some(v) = &self.version {
            inline_table.push(("version", Value::from(v.clone())));
        }

        if self.export {
            inline_table.push(("export", Value::from(true)));
        }

        deps[&self.name] = value(InlineTable::from_iter(inline_table));

        if self.dry_run {
            println!("{}", document.to_string());
        } else {
            fs::write(&manifest_path, document.to_string()).with_context(|| {
                format!(
                    "failed to write manifest file `{}`",
                    manifest_path.display()
                )
            })?;
        }

        Ok(())
    }

    fn validate(&self, metadata: &ComponentMetadata) -> Result<()> {
        if self.export {
            // Validate default export
            if let Some(default) = &metadata.interface {
                if self.version.is_none() || self.name == default.interface.name {
                    bail!(
                        "{} is already declared as the default interface",
                        default.interface.name
                    );
                }
            }
        }

        // Validate exports
        let exports: Vec<&str> = metadata
            .exports
            .iter()
            .map(|e| e.interface.name.as_str())
            .collect();

        if exports.contains(&self.name.as_str()) {
            bail!("{} is already declared as an export", self.name);
        }

        // Validate imports
        let imports: Vec<&str> = metadata
            .imports
            .iter()
            .map(|i| i.interface.name.as_str())
            .collect();

        if imports.contains(&self.name.as_str()) {
            bail!("{} is already declared as an import", self.name);
        }

        Ok(())
    }
}
