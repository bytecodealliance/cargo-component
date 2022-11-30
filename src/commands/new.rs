use anyhow::{bail, Context, Result};
use cargo::{
    ops::{self, NewOptions, VersionControl},
    Config,
};
use clap::{ArgAction, Args};
use heck::{ToKebabCase, ToSnakeCase, ToUpperCamelCase};
use std::{
    borrow::Cow,
    fmt, fs,
    path::{Path, PathBuf},
};
use toml_edit::{table, value, Document, InlineTable, Item, Table, Value};

use crate::WIT_BINDGEN_REPO;

fn is_wit_keyword(s: &str) -> bool {
    // TODO: move this into wit-parser?
    matches!(
        s,
        "use"
            | "type"
            | "func"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "s8"
            | "s16"
            | "s32"
            | "s64"
            | "float32"
            | "float64"
            | "char"
            | "record"
            | "list"
            | "flags"
            | "variant"
            | "enum"
            | "union"
            | "bool"
            | "string"
            | "option"
            | "result"
            | "future"
            | "stream"
            | "as"
            | "from"
            | "static"
            | "interface"
            | "tuple"
            | "implements"
            | "import"
            | "export"
            | "world"
            | "default"
    )
}

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
    kebab: String,
    snake: String,
    camel: String,
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
        let camel = package.to_upper_camel_case();

        if kebab.is_empty() || snake.is_empty() || camel.is_empty() {
            bail!("invalid component name `{package}`");
        }

        wit_parser::validate_id(&kebab)
            .with_context(|| format!("component name `{package}` is not a legal WIT identifier"))?;

        if is_rust_keyword(&snake) {
            bail!("component name `{package}` cannot be used as it is a Rust keyword");
        }

        Ok(Self {
            display,
            kebab,
            snake,
            camel,
        })
    }
}

impl fmt::Display for PackageName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display)
    }
}

impl NewCommand {
    /// Executes the command.
    pub fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing new command");

        let name = PackageName::new(self.name.as_deref(), &self.path)?;

        config.configure(
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

        ops::new(&opts, config)?;

        let out_dir = config.cwd().join(&self.path);
        self.update_manifest(&name, &out_dir)?;
        self.update_source_file(&name, &out_dir)?;
        self.create_interface_file(&name, &out_dir)?;
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
            config.cwd().join(&self.path),
            self.name.clone(),
            self.edition.clone(),
            None,
        )
    }

    fn update_manifest(&self, name: &PackageName, out_dir: &Path) -> Result<()> {
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

        let mut exports = Table::new();
        exports[&name.snake] = value(
            Path::new(&name.snake)
                .with_extension("wit")
                .to_string_lossy()
                .as_ref(),
        );

        let mut component = Table::new();
        component.set_implicit(true);
        component["direct-export"] = value(&name.snake);
        component["exports"] = Item::Table(exports);

        let mut metadata = Table::new();
        metadata.set_implicit(true);
        metadata.set_position(doc.len());
        metadata["component"] = Item::Table(component);

        doc["package"]["metadata"] = Item::Table(metadata);
        doc["dependencies"]["wit-bindgen-guest-rust"] = value(InlineTable::from_iter(
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

    fn update_source_file(&self, name: &PackageName, out_dir: &Path) -> Result<()> {
        let source_path = out_dir.join("src/lib.rs");
        fs::write(
            &source_path,
            format!(
                r#"use bindings::{snake};

struct Component;

impl {snake}::{camel} for Component {{
    fn hello_world() -> String {{
        "Hello, World!".to_string()
    }}
}}

bindings::export!(Component);
"#,
                snake = name.snake,
                camel = name.camel
            ),
        )
        .with_context(|| format!("failed to write source file `{}`", source_path.display()))
    }

    fn create_interface_file(&self, name: &PackageName, out_dir: &Path) -> Result<()> {
        let mut interface_path = out_dir.join(&name.snake);
        interface_path.set_extension("wit");

        fs::write(
            &interface_path,
            format!(
                r#"interface {percent}{kebab} {{
    hello-world: func() -> string
}}
"#,
                percent = if is_wit_keyword(&name.kebab) { "%" } else { "" },
                kebab = name.kebab,
            ),
        )
        .with_context(|| {
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
