use crate::Config;
use anyhow::{bail, Context, Result};
use cargo::ops::{self, NewOptions, VersionControl};
use clap::{ArgAction, Args};
use heck::{ToKebabCase, ToSnakeCase};
use std::{
    borrow::Cow,
    fmt, fs,
    path::{Path, PathBuf},
};
use toml_edit::{table, value, Document, InlineTable, Item, Table, Value};

fn is_rust_keyword(s: &str) -> bool {
    // TODO: source this from somewhere?
    matches!(
        s,
        "as" 
            | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            // Reserved for future use
            | "abstract"
            | "become"
            | "box"
            | "do"
            | "final"
            | "macro"
            | "override"
            | "priv"
            | "try"
            | "typeof"
            | "unsized"
            | "virtual"
            | "yield"
    )
}

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
    #[clap(long = "vcs", value_name = "VCS", value_parser = ["git", "hg", "pijul", "fossil", "none"])]
    pub vcs: Option<String>,

    /// Use verbose output (-vv very verbose/build.rs output)
    #[clap(
        long = "verbose",
        short = 'v',
        action = ArgAction::Count
    )]
    pub verbose: u8,

    ///  Use a library template [default]
    #[clap(long = "lib")]
    pub lib: bool,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

    /// Edition to set for the generated crate
    #[clap(long = "edition", value_name = "YEAR", value_parser = ["2015", "2018", "2021"])]
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
    #[clap(long = "editor", value_name = "EDITOR", value_parser = ["vscode", "none"])]
    pub editor: Option<String>,

    /// The path for the generated package.
    #[clap(value_name = "path")]
    pub path: PathBuf,
}

struct PackageName<'a> {
    display: Cow<'a, str>,
}

impl<'a> PackageName<'a> {
    fn new(name: Option<&'a str>, path: &'a Path) -> Result<Self> {
        let (package, display) = match name {
            Some(name) => (name.into(), name.into()),
            None => (
                path.file_name().expect("invalid path").to_string_lossy(),
                // `cargo new` prints the given path to the new package, so
                // use the path for the display value.
                path.as_os_str().to_string_lossy(),
            ),
        };

        let kebab = package.to_kebab_case();
        let snake = package.to_snake_case();

        if kebab.is_empty() || snake.is_empty() {
            bail!("invalid component name `{package}`");
        }

        wit_parser::validate_id(&kebab)
            .with_context(|| format!("component name `{package}` is not a legal WIT identifier"))?;

        if is_rust_keyword(&snake) {
            bail!("component name `{package}` cannot be used as it is a Rust keyword");
        }

        Ok(Self { display })
    }
}

impl fmt::Display for PackageName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{name}", name = self.display)
    }
}

impl NewCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing new command");

        let name = PackageName::new(self.name.as_deref(), &self.path)?;

        config.cargo_mut().configure(
            u32::from(self.verbose),
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

        ops::new(&opts, config.cargo())?;

        let out_dir = config.cargo().cwd().join(&self.path);
        self.update_manifest(&out_dir)?;
        self.update_source_file(&out_dir)?;
        self.create_targets_file(&out_dir)?;
        self.create_editor_settings_file(&out_dir)?;

        // `cargo new` prints the given path to the new package, so
        // do the same here.
        config
            .shell()
            .status("Created", format!("component `{name}` package"))?;

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
            config.cargo().cwd().join(&self.path),
            self.name.clone(),
            self.edition.clone(),
            None,
        )
    }

    fn update_manifest(&self, out_dir: &Path) -> Result<()> {
        let manifest_path = out_dir.join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path).with_context(|| {
            format!(
                "failed to read manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        let mut doc: Document = manifest.parse().with_context(|| {
            format!(
                "failed to parse manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        doc["lib"] = table();
        doc["lib"]["crate-type"] = value(Value::from_iter(["cdylib"].into_iter()));

        let mut component = Table::new();
        component.set_implicit(true);
        component["target"] = value(InlineTable::from_iter(
            [("path", Value::from("world.wit"))].into_iter(),
        ));
        component["dependencies"] = Item::Table(Table::new());

        let mut metadata = Table::new();
        metadata.set_implicit(true);
        metadata.set_position(doc.len());
        metadata["component"] = Item::Table(component);

        doc["package"]["metadata"] = Item::Table(metadata);
        doc["dependencies"]["wit-bindgen"] = value(InlineTable::from_iter(
            [
                ("version", Value::from("0.3.0")),
                ("default_features", Value::from(false)),
            ]
            .into_iter(),
        ));

        fs::write(&manifest_path, doc.to_string()).with_context(|| {
            format!(
                "failed to write manifest file `{path}`",
                path = manifest_path.display()
            )
        })
    }

    fn update_source_file(&self, out_dir: &Path) -> Result<()> {
        let source_path = out_dir.join("src/lib.rs");
        fs::write(
            &source_path,
            r#"
struct Component;

impl bindings::Component for Component {
    fn hello_world() -> String {
        "Hello, World!".to_string()
    }
}

bindings::export!(Component);
"#,
        )
        .with_context(|| {
            format!(
                "failed to write source file `{path}`",
                path = source_path.display()
            )
        })
    }

    fn create_targets_file(&self, out_dir: &Path) -> Result<()> {
        let path = out_dir.join("world.wit");

        fs::write(
            &path,
            r#"/// An example world for the component to target.
default world component {
    export hello-world: func() -> string
}                
"#,
        )
        .with_context(|| {
            format!(
                "failed to write targets file `{path}`",
                path = path.display()
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
                        "failed to write editor settings file `{path}`",
                        path = settings_path.display()
                    )
                })
            }
            Some("none") => Ok(()),
            _ => unreachable!(),
        }
    }
}
