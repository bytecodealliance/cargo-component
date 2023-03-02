use crate::{
    bindings::SourceGenerator,
    metadata,
    registry::{self, ContentLocation, RegistryPackageResolution},
    Config,
};
use anyhow::{anyhow, bail, Context, Result};
use cargo::ops::{self, NewOptions, VersionControl};
use clap::{ArgAction, Args};
use heck::ToKebabCase;
use semver::VersionReq;
use std::{
    borrow::Cow,
    collections::HashMap,
    fmt, fs,
    path::{Path, PathBuf},
};
use toml_edit::{table, value, Document, InlineTable, Item, Table, Value};
use url::Url;

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

    /// Use a library template [default]
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

    /// Use the specified target WIT package.
    #[clap(long = "target", short = 't', value_name = "TARGET")]
    pub target: Option<String>,

    /// Use the specified world within the target WIT package.
    #[clap(long = "world", short = 'w', value_name = "WORLD", requires = "target")]
    pub world: Option<String>,

    /// Use the specified default registry when generating the package.
    #[clap(long = "registry", value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// Disable the use of `rustfmt` when generating source code.
    #[clap(long = "no-rustfmt")]
    pub no_rustfmt: bool,

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

        if kebab.is_empty() {
            bail!("invalid component name `{package}`");
        }

        wit_parser::validate_id(&kebab)
            .with_context(|| format!("component name `{package}` is not a legal WIT identifier"))?;

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

        let out_dir = config.cargo().cwd().join(&self.path);
        let registries = self.registries()?;
        let target = self.resolve_target(config, &registries).await?;
        let source = self.generate_source(&target)?;

        let opts = self.new_options(config)?;
        ops::new(&opts, config.cargo())?;

        // `cargo new` prints the given path to the new package, so
        // do the same here.
        config
            .shell()
            .status("Created", format!("component `{name}` package"))?;

        self.update_manifest(&out_dir, &registries, &target)?;
        self.create_source_file(config, &out_dir, source.as_ref(), &target)?;
        self.create_targets_file(&out_dir)?;
        self.create_editor_settings_file(&out_dir)?;

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

    fn update_manifest(
        &self,
        out_dir: &Path,
        registries: &HashMap<String, metadata::Registry>,
        target: &Option<RegistryPackageResolution>,
    ) -> Result<()> {
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
        component["target"] = match target.as_ref() {
            Some(target) => value(format!(
                "{id}@{version}",
                id = target.id,
                version = target.version
            )),
            None => value(InlineTable::from_iter(
                [("path", Value::from("world.wit"))].into_iter(),
            )),
        };
        component["dependencies"] = Item::Table(Table::new());

        if !registries.is_empty() {
            let mut table = Table::new();
            for (name, reg) in registries {
                table[name] = match reg {
                    metadata::Registry::Remote(url) => value(url.to_string()),
                    metadata::Registry::Local(path) => value(InlineTable::from_iter(
                        [(
                            "path",
                            Value::from(Path::new("..").join(path).to_str().ok_or_else(|| {
                                anyhow!(
                                    "invalid path `{path}` for local registry",
                                    path = path.display()
                                )
                            })?),
                        )]
                        .into_iter(),
                    )),
                }
            }
            component["registries"] = Item::Table(table);
        }

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

    fn generate_source(&self, target: &Option<RegistryPackageResolution>) -> Result<Cow<str>> {
        match target {
            Some(target) => {
                let path: Cow<Path> = match &target.location {
                    ContentLocation::Local(path) => path.into(),
                    ContentLocation::Remote(_) => todo!("support remote content"),
                };
                let generator = SourceGenerator::new(&target.id, path.as_ref(), !self.no_rustfmt);
                generator.generate(self.world.as_deref()).map(Into::into)
            }
            None => Ok(r#"struct Component;

impl bindings::Component for Component {
    fn hello_world() -> String {
        "Hello, World!".to_string()
    }
}

bindings::export!(Component);
"#
            .into()),
        }
    }

    fn create_source_file(
        &self,
        config: &Config,
        out_dir: &Path,
        source: &str,
        target: &Option<RegistryPackageResolution>,
    ) -> Result<()> {
        match target {
            Some(target) => {
                config.shell().status(
                    "Generating",
                    format!(
                        "source file for target `{id}` v{version}",
                        id = target.id,
                        version = target.version
                    ),
                )?;
            }
            None => {
                config
                    .shell()
                    .status("Generating", "\"hello world\" example source file")?;
            }
        }

        let source_path = out_dir.join("src/lib.rs");
        fs::write(&source_path, source).with_context(|| {
            format!(
                "failed to write source file `{path}`",
                path = source_path.display()
            )
        })
    }

    fn create_targets_file(&self, out_dir: &Path) -> Result<()> {
        if self.target.is_some() {
            return Ok(());
        }

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

    async fn resolve_target(
        &self,
        config: &Config,
        registries: &HashMap<String, metadata::Registry>,
    ) -> Result<Option<RegistryPackageResolution>> {
        match self.target.as_ref() {
            Some(target) => {
                let (id, requirement) = if target.contains('@') {
                    let package: metadata::RegistryPackage = target.parse()?;
                    (package.id, package.version)
                } else {
                    (target.as_str().into(), VersionReq::STAR)
                };

                let registry = registry::create(config, None, registries)?;

                config
                    .cargo()
                    .shell()
                    .status("Updating", "component registry logs")?;

                registry.synchronize(&[&id]).await?;

                match registry.resolve(&id, &requirement)? {
                    Some(target) => Ok(Some(target)),
                    None => bail!("a version of package `{id}` that satisfies version requirement `{requirement}` was not found")
                }
            }
            None => Ok(None),
        }
    }

    fn registries(&self) -> Result<HashMap<String, metadata::Registry>> {
        let mut registries = HashMap::new();

        if let Some(registry) = self.registry.as_deref() {
            // Check if the specified registry exists as a path
            let registry = if Path::new(registry).exists() {
                metadata::Registry::Local(registry.into())
            } else {
                match Url::try_from(registry) {
                    Ok(url) => metadata::Registry::Remote(url),
                    Err(_) => bail!("local registry `{registry}` does not exist"),
                }
            };
            registries.insert("default".to_string(), registry);
        }

        Ok(registries)
    }
}
