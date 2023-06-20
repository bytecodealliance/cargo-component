use crate::{
    bindings::WIT_BINDGEN_VERSION,
    generator::SourceGenerator,
    metadata,
    registry::{DependencyResolution, DependencyResolver, RegistryResolution},
    Config,
};
use anyhow::{bail, Context, Result};
use cargo::ops::{self, NewOptions, VersionControl};
use clap::{ArgAction, Args};
use heck::ToKebabCase;
use semver::VersionReq;
use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use toml_edit::{table, value, Document, InlineTable, Item, Table, Value};
use url::Url;

fn escape_wit(s: &str) -> Cow<str> {
    match s {
        "use" | "type" | "func" | "u8" | "u16" | "u32" | "u64" | "s8" | "s16" | "s32" | "s64"
        | "float32" | "float64" | "char" | "record" | "flags" | "variant" | "enum" | "union"
        | "bool" | "string" | "option" | "result" | "future" | "stream" | "list" | "_" | "as"
        | "from" | "static" | "interface" | "tuple" | "import" | "export" | "world" | "package" => {
            Cow::Owned(format!("%{s}"))
        }
        _ => s.into(),
    }
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

    /// Use a binary (command) template [default]
    #[clap(long = "bin", conflicts_with("lib"))]
    pub bin: bool,

    /// Use a library (reactor) template
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

    /// The component package namespace to use.
    #[clap(
        long = "namespace",
        value_name = "NAMESPACE",
        default_value = "component"
    )]
    pub namespace: String,

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

    /// Use the specified target world from a WIT package.
    #[clap(long = "target", short = 't', value_name = "TARGET", requires = "lib")]
    pub target: Option<String>,

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
    namespace: String,
    name: String,
    display: Cow<'a, str>,
}

impl<'a> PackageName<'a> {
    fn new(namespace: &str, name: Option<&'a str>, path: &'a Path) -> Result<Self> {
        let (name, display) = match name {
            Some(name) => (name.into(), name.into()),
            None => (
                path.file_name().expect("invalid path").to_string_lossy(),
                // `cargo new` prints the given path to the new package, so
                // use the path for the display value.
                path.as_os_str().to_string_lossy(),
            ),
        };

        let namespace_kebab = namespace.to_kebab_case();
        if namespace_kebab.is_empty() {
            bail!("invalid component namespace `{namespace}`");
        }

        wit_parser::validate_id(&namespace_kebab).with_context(|| {
            format!("component namespace `{namespace}` is not a legal WIT identifier")
        })?;

        let name_kebab = name.to_kebab_case();
        if name_kebab.is_empty() {
            bail!("invalid component name `{name}`");
        }

        wit_parser::validate_id(&name_kebab)
            .with_context(|| format!("component name `{name}` is not a legal WIT identifier"))?;

        Ok(Self {
            namespace: namespace_kebab,
            name: name_kebab,
            display,
        })
    }
}

impl NewCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing new command");

        let name = PackageName::new(&self.namespace, self.name.as_deref(), &self.path)?;

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

        let target: Option<metadata::Target> = match self.target.as_deref() {
            Some(s) if s.contains('@') => Some(s.parse()?),
            Some(s) => Some(format!("{s}@{version}", version = VersionReq::STAR).parse()?),
            None => None,
        };

        let target = self.resolve_target(config, &registries, target).await?;
        let source = self.generate_source(&target)?;

        let opts = self.new_options(config)?;
        ops::new(&opts, config.cargo())?;

        config.shell().status(
            "Created",
            format!("component `{name}` package", name = name.display),
        )?;

        self.update_manifest(&name, &out_dir, &registries, &target)?;
        self.create_source_file(config, &out_dir, source.as_ref(), &target)?;
        self.create_targets_file(&name, &out_dir)?;
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
            self.bin,
            self.lib,
            config.cargo().cwd().join(&self.path),
            self.name.clone(),
            self.edition.clone(),
            None,
        )
    }

    fn update_manifest(
        &self,
        name: &PackageName,
        out_dir: &Path,
        registries: &HashMap<String, Url>,
        target: &Option<(RegistryResolution, Option<String>)>,
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

        if !self.is_bin() {
            doc["lib"] = table();
            doc["lib"]["crate-type"] = value(Value::from_iter(["cdylib"].into_iter()));
        }

        let mut component = Table::new();
        component.set_implicit(true);

        component["package"] = value(format!(
            "{ns}:{name}",
            ns = name.namespace,
            name = name.name
        ));

        if !self.is_bin() {
            component["target"] = match target.as_ref() {
                Some((resolution, world)) => match world {
                    Some(world) => value(format!(
                        "{id}/{world}@{version}",
                        id = resolution.id,
                        version = resolution.version
                    )),
                    None => value(format!(
                        "{id}@{version}",
                        id = resolution.id,
                        version = resolution.version
                    )),
                },
                None => {
                    let mut target_deps = Table::new();
                    target_deps["path"] = value("wit");
                    Item::Table(target_deps)
                }
            };
        }

        component["dependencies"] = Item::Table(Table::new());

        if !registries.is_empty() {
            let mut table = Table::new();
            for (name, url) in registries {
                table[name] = value(url.as_str());
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
                ("version", Value::from(WIT_BINDGEN_VERSION)),
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

    fn is_bin(&self) -> bool {
        self.bin || !self.lib
    }

    fn generate_source(
        &self,
        target: &Option<(RegistryResolution, Option<String>)>,
    ) -> Result<Cow<str>> {
        if self.is_bin() {
            // Return empty source here to avoid creating a new source file
            // As a result, whatever source that was generated by `cargo new` will be kept
            return Ok("".into());
        }

        match target {
            Some((resolution, world)) => {
                let generator =
                    SourceGenerator::new(&resolution.id, &resolution.path, !self.no_rustfmt);
                generator.generate(world.as_deref()).map(Into::into)
            }
            None => Ok(r#"struct Component;

impl bindings::Example for Component {
    /// Say hello!
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
        target: &Option<(RegistryResolution, Option<String>)>,
    ) -> Result<()> {
        if source.is_empty() {
            return Ok(());
        }

        match target {
            Some((resolution, _)) => {
                config.shell().status(
                    "Generating",
                    format!(
                        "source file for target `{id}` v{version}",
                        id = resolution.id,
                        version = resolution.version
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

    fn create_targets_file(&self, name: &PackageName, out_dir: &Path) -> Result<()> {
        if self.is_bin() || self.target.is_some() {
            return Ok(());
        }

        let wit_path = out_dir.join("wit");
        fs::create_dir(&wit_path).with_context(|| {
            format!(
                "failed to create targets directory `{wit_path}`",
                wit_path = wit_path.display()
            )
        })?;

        let path = wit_path.join("world.wit");

        fs::write(
            &path,
            format!(
                r#"package {ns}:{pkg}

/// An example world for the component to target.
world example {{
    export hello-world: func() -> string
}}                
"#,
                ns = escape_wit(&name.namespace),
                pkg = escape_wit(&name.name),
            ),
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
        registries: &HashMap<String, Url>,
        target: Option<metadata::Target>,
    ) -> Result<Option<(RegistryResolution, Option<String>)>> {
        match target {
            Some(metadata::Target::Package { id, package, world }) => {
                let mut resolver = DependencyResolver::new(config, registries, None);
                let dependency = metadata::Dependency::Package(package);

                resolver.add_dependency(&id, &dependency, true).await?;

                let (target, dependencies) = resolver.resolve().await?;
                assert_eq!(target.len(), 1);
                assert!(dependencies.is_empty());

                match target
                    .into_values()
                    .next()
                    .expect("expected a target resolution")
                {
                    DependencyResolution::Registry(resolution) => Ok(Some((resolution, world))),
                    _ => unreachable!(),
                }
            }
            _ => Ok(None),
        }
    }

    fn registries(&self) -> Result<HashMap<String, Url>> {
        let mut registries = HashMap::new();

        if let Some(url) = self.registry.as_deref() {
            registries.insert(
                "default".to_string(),
                url.parse().context("failed to parse registry URL")?,
            );
        }

        Ok(registries)
    }
}
