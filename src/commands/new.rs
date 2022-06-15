use anyhow::{Context, Result};
use cargo::{
    ops::{self, NewOptions, VersionControl},
    Config,
};
use clap::Args;
use std::{fs, path::Path};
use toml_edit::{table, value, Document, InlineTable, Item, Table, Value};

use crate::WIT_BINDGEN_REPO;

/// Create a new WebAssembly component package at <path>
#[derive(Args)]
pub struct NewCommand {
    /// Do not print cargo log messages
    #[clap(long = "quiet", short = 'q')]
    pub quiet: bool,

    /// Initialize a new repository for the given version
    /// control system (git, hg, pijul, or fossil) or do not
    /// initialize any version control at all (none), overriding
    /// a global configuration.
    #[clap(long = "vcs", value_name = "VCS", possible_values = ["git", "hg", "pijul", "fossil", "none"])]
    pub vcs: Option<String>,

    /// Use verbose output (-vv very verbose/build.rs output)
    #[clap(
        long = "verbose",
        short = 'v',
        takes_value = false,
        parse(from_occurrences)
    )]
    pub verbose: u32,

    ///  Use a library template [default]
    #[clap(long = "lib")]
    pub lib: bool,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

    /// Edition to set for the generated crate
    #[clap(long = "edition", value_name = "YEAR", possible_values = ["2015", "2018", "2021"])]
    pub edition: Option<String>,

    /// Require Cargo.lock and cache are up to date
    #[clap(long = "frozen")]
    pub frozen: bool,

    /// Set the resulting package name, defaults to the directory name
    #[clap(long = "name", value_name = "NAME")]
    pub name: Option<String>,

    /// Require Cargo.lock is up to date
    #[clap(long = "locked")]
    pub locked: bool,

    /// Run without accessing the network
    #[clap(long = "offline")]
    pub offline: bool,

    /// Code editor to use for rust-analyzer integration, defaults to `vscode`
    #[clap(long = "editor", value_name = "EDITOR", possible_values = ["vscode", "none"])]
    pub editor: Option<String>,

    /// The path for the generated package.
    #[clap(value_name = "path")]
    pub path: String,
}

impl NewCommand {
    /// Executes the command.
    pub fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing new command");

        config.configure(
            self.verbose,
            self.quiet,
            self.color.as_deref(),
            self.frozen,
            self.locked,
            self.offline,
            &None,
            &[],
            &[],
        )?;

        let opts = self.new_options(config)?;

        ops::new(&opts, config)?;

        let out_dir = config.cwd().join(&self.path);
        self.update_manifest(&out_dir)?;
        self.update_source_file(&out_dir)?;
        self.create_interface_file(&out_dir)?;
        self.create_editor_settings_file(&out_dir)?;

        let package_name = if let Some(name) = &self.name {
            name
        } else {
            &self.path
        };

        config
            .shell()
            .status("Created", format!("component `{}` package", package_name))?;

        Ok(())
    }

    fn new_options(&self, config: &Config) -> Result<NewOptions> {
        let vcs = self.vcs.as_deref().map(|vcs| match vcs {
            "git" => VersionControl::Git,
            "hg" => VersionControl::Hg,
            "pijul" => VersionControl::Pijul,
            "fossil" => VersionControl::Fossil,
            "none" => VersionControl::NoVcs,
            _ => unreachable!(),
        });

        NewOptions::new(
            vcs,
            false,
            true,
            config.cwd().join(&self.path),
            self.name.clone(),
            self.edition.clone(),
            None,
        )
    }

    fn update_manifest(&self, out_dir: &Path) -> Result<()> {
        let manifest_path = out_dir.join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path).with_context(|| {
            format!("failed to read manifest file `{}`", manifest_path.display())
        })?;

        let mut doc: Document = manifest.parse().with_context(|| {
            format!(
                "failed to parse manifest file `{}`",
                manifest_path.display()
            )
        })?;

        doc["lib"] = table();
        doc["lib"]["crate-type"] = value(Value::from_iter(["cdylib"].into_iter()));

        let interface = InlineTable::from_iter(
            [
                ("path", Value::from("interface.wit")),
                ("export", Value::from(true)),
            ]
            .into_iter(),
        );

        let mut dependencies = Table::new();
        dependencies["interface"] = Item::Value(Value::InlineTable(interface));

        let mut component = Table::new();
        component.set_implicit(true);
        component["dependencies"] = Item::Table(dependencies);

        let mut metadata = Table::new();
        metadata.set_implicit(true);
        metadata.set_position(doc.len());
        metadata["component"] = Item::Table(component);

        doc["package"]["metadata"] = Item::Table(metadata);
        doc["dependencies"]["wit-bindgen-rust"] = value(InlineTable::from_iter(
            [
                ("git", Value::from(WIT_BINDGEN_REPO)),
                ("default_features", Value::from(false)),
            ]
            .into_iter(),
        ));

        fs::write(&manifest_path, doc.to_string()).with_context(|| {
            format!(
                "failed to write manifest file `{}`",
                manifest_path.display()
            )
        })
    }

    fn update_source_file(&self, out_dir: &Path) -> Result<()> {
        const DEFAULT_SOURCE_FILE: &str = r#"use interface::Interface;

struct Component;

impl Interface for Component {
    fn say_something() -> String {
        "Hello, World!".to_string()
    }
}

interface::export!(Component);
"#;

        let source_path = out_dir.join("src/lib.rs");
        fs::write(&source_path, DEFAULT_SOURCE_FILE)
            .with_context(|| format!("failed to write source file `{}`", source_path.display()))
    }

    fn create_interface_file(&self, out_dir: &Path) -> Result<()> {
        const DEFAULT_INTERFACE_FILE: &str = "say-something: func() -> string\n";

        let interface_path = out_dir.join("interface.wit");
        fs::write(&interface_path, DEFAULT_INTERFACE_FILE).with_context(|| {
            format!(
                "failed to write interface file `{}`",
                interface_path.display()
            )
        })
    }

    fn create_editor_settings_file(&self, out_dir: &Path) -> Result<()> {
        match self.editor.as_deref() {
            Some("vscode") | None => {
                let settings_dir = out_dir.join(".vscode");
                let settings_path = settings_dir.join("settings.json");

                fs::create_dir_all(settings_dir)?;

                fs::write(
                    &settings_path,
                    r#"{
    "rust-analyzer.server.extraEnv": { "CARGO": "cargo-component" }
}
"#,
                )
                .with_context(|| {
                    format!(
                        "failed to write editor settings file `{}`",
                        settings_path.display()
                    )
                })
            }
            Some("none") => Ok(()),
            _ => unreachable!(),
        }
    }
}
