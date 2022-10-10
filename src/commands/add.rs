use super::workspace;
use crate::ComponentMetadata;
use anyhow::{bail, Context, Result};
use cargo::{core::package::Package, ops::Packages, Config};
use clap::{ArgAction, Args};
use std::{fs, path::PathBuf};
use toml_edit::{table, value, Document};

/// Add a dependency for a WebAssembly component
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
        action = ArgAction::Count
    )]
    pub verbose: u8,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

    /// Path to the interface definition of the dependency
    #[clap(long = "path", value_name = "PATH")]
    pub path: String,

    /// Name of the dependency
    #[clap(value_name = "name")]
    pub name: String,

    /// Add the dependency as an exported interface
    #[clap(long = "export")]
    pub export: bool,

    /// Sets the dependency as the directly exported interface (implies `--export`).
    #[clap(long = "direct-export")]
    pub direct_export: bool,

    /// Path to the manifest to add a dependency to
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Don't actually write the manifest
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// Package to add the dependency to (see `cargo help pkgid`)
    #[clap(long = "package", short = 'p', value_name = "SPEC")]
    pub package: Option<String>,
}

impl AddCommand {
    /// Executes the command
    pub fn exec(mut self, config: &mut Config) -> Result<()> {
        if self.direct_export {
            self.export = true;
        }

        config.configure(
            u32::from(self.verbose),
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
        let package = if let Some(ref inner) = self.package {
            let pkg = Packages::from_flags(false, vec![], vec![inner.clone()])?;
            pkg.get_packages(&ws)?[0]
        } else {
            ws.current()?
        };

        let component_metadata = ComponentMetadata::from_package(config, package)?;

        self.validate(package, &component_metadata)?;
        self.add(package)?;

        config.shell().status(
            "Adding",
            format!(
                "interface {name} to {ty}",
                name = self.name,
                ty = if self.export { "exports" } else { "imports" }
            ),
        )?;

        Ok(())
    }

    fn add(&self, pkg: &Package) -> Result<()> {
        let manifest_path = pkg.manifest_path();
        let manifest = fs::read_to_string(&manifest_path).with_context(|| {
            format!("failed to read manifest file `{}`", manifest_path.display())
        })?;

        let mut document: Document = manifest.parse().with_context(|| {
            format!(
                "failed to parse manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        let component = &mut document["package"]["metadata"]["component"]
            .as_table_mut()
            .with_context(|| {
                format!(
                    "failed to find component metadata in manifest file `{path}`",
                    path = manifest_path.display()
                )
            })?;

        if self.direct_export {
            component["direct-interface-export"] = value(&self.name);
        }

        let deps = if self.export {
            component.entry("exports").or_insert_with(table)
        } else {
            component.entry("imports").or_insert_with(table)
        };

        deps[&self.name] = value(&self.path);

        if self.dry_run {
            println!("{}", document);
        } else {
            fs::write(&manifest_path, document.to_string()).with_context(|| {
                format!(
                    "failed to write manifest file `{path}`",
                    path = manifest_path.display()
                )
            })?;
        }

        Ok(())
    }

    fn validate(&self, package: &Package, metadata: &ComponentMetadata) -> Result<()> {
        if package.name() == self.name.as_str() {
            bail!(
                "cannot add dependency `{name}` as it conflicts with the package name",
                name = self.name
            );
        }

        if package
            .manifest()
            .dependencies()
            .iter()
            .any(|d| d.name_in_toml() == self.name.as_str())
        {
            bail!(
                "a crate dependency with name `{name}` already exists",
                name = self.name,
            );
        }

        if metadata
            .imports
            .iter()
            .any(|d| d.interface.name == self.name)
        {
            bail!(
                "an import with name `{name}` already exists",
                name = self.name,
            );
        }

        if metadata
            .exports
            .iter()
            .any(|d| d.interface.name == self.name)
        {
            bail!(
                "an export with name `{name}` already exists",
                name = self.name,
            );
        }

        let path = PathBuf::from(&self.path);
        if !path.exists() {
            bail!(
                "interface file `{path}` does not exist or is not a file",
                path = path.display()
            );
        }

        if self.direct_export && metadata.direct_export.is_some() {
            bail!("a directly exported interface has already been specified in the manifest");
        }

        Ok(())
    }
}
