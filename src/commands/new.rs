use crate::{config::Config, generator::SourceGenerator, metadata, metadata::DEFAULT_WIT_DIR};
use anyhow::{bail, Context, Result};
use cargo_component_core::{
    command::CommonOptions,
    registry::{Dependency, DependencyResolution, DependencyResolver, RegistryResolution},
};
use clap::Args;
use heck::ToKebabCase;
use semver::VersionReq;
use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use toml_edit::{table, value, Array, Document, InlineTable, Item, Table, Value};
use url::Url;

const WIT_BINDGEN_CRATE: &str = "wit-bindgen";
const WIT_BINDGEN_VERSION: &str = "0.16.0";

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
#[clap(disable_version_flag = true)]
pub struct NewCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Initialize a new repository for the given version
    /// control system (git, hg, pijul, or fossil) or do not
    /// initialize any version control at all (none), overriding
    /// a global configuration.
    #[clap(long = "vcs", value_name = "VCS", value_parser = ["git", "hg", "pijul", "fossil", "none"])]
    pub vcs: Option<String>,

    /// Create a CLI command component [default]
    #[clap(long = "command", conflicts_with("lib"))]
    pub command: bool,

    /// Create a library (reactor) component
    #[clap(long = "lib", alias = "reactor")]
    pub lib: bool,

    /// Edition to set for the generated crate
    #[clap(long = "edition", value_name = "YEAR", value_parser = ["2015", "2018", "2021"])]
    pub edition: Option<String>,

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

    /// Code editor to use for rust-analyzer integration, defaults to `vscode`
    #[clap(long = "editor", value_name = "EDITOR", value_parser = ["emacs", "vscode", "none"])]
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
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing new command");

        let config = Config::new(self.common.new_terminal())?;

        let name = PackageName::new(&self.namespace, self.name.as_deref(), &self.path)?;

        let out_dir = std::env::current_dir()
            .with_context(|| "couldn't get the current directory of the process")?
            .join(&self.path);
        let registries = self.registries()?;

        let target: Option<metadata::Target> = match self.target.as_deref() {
            Some(s) if s.contains('@') => Some(s.parse()?),
            Some(s) => Some(format!("{s}@{version}", version = VersionReq::STAR).parse()?),
            None => None,
        };

        let target = self
            .resolve_target(&config, &registries, target, true)
            .await?;
        let source = self.generate_source(&target)?;

        let mut command = self.new_command();
        match command.status() {
            Ok(status) => {
                if !status.success() {
                    std::process::exit(status.code().unwrap_or(1));
                }
            }
            Err(e) => {
                bail!("failed to execute `cargo new` command: {e}")
            }
        }

        self.update_manifest(&config, &name, &out_dir, &registries, &target)?;
        self.create_source_file(&config, &out_dir, source.as_ref(), &target)?;
        self.create_targets_file(&name, &out_dir)?;
        self.create_editor_settings_file(&out_dir)?;

        Ok(())
    }

    fn new_command(&self) -> Command {
        let mut command = std::process::Command::new("cargo");
        command.arg("new");

        if let Some(name) = &self.name {
            command.arg("--name").arg(name);
        }

        if let Some(edition) = &self.edition {
            command.arg("--edition").arg(edition);
        }

        if let Some(vcs) = &self.vcs {
            command.arg("--vcs").arg(vcs);
        }

        if self.common.quiet {
            command.arg("-q");
        }

        command.args(std::iter::repeat("-v").take(self.common.verbose as usize));

        if let Some(color) = self.common.color {
            command.arg("--color").arg(color.to_string());
        }

        if !self.is_command() {
            command.arg("--lib");
        }

        command.arg(&self.path);
        command
    }

    fn update_manifest(
        &self,
        config: &Config,
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

        if !self.is_command() {
            doc["lib"] = table();
            doc["lib"]["crate-type"] = value(Value::from_iter(["cdylib"]));
        }

        let mut component = Table::new();
        component.set_implicit(true);

        component["package"] = value(format!(
            "{ns}:{name}",
            ns = name.namespace,
            name = name.name
        ));

        if !self.is_command() {
            if let Some((resolution, world)) = target.as_ref() {
                component["target"] = match world {
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
                };
            }
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
        doc["dependencies"][WIT_BINDGEN_CRATE] = value(InlineTable::from_iter([
            ("version", Value::from(WIT_BINDGEN_VERSION)),
            ("default-features", Value::from(false)),
            ("features", Value::from(Array::from_iter(["realloc"]))),
        ]));

        fs::write(&manifest_path, doc.to_string()).with_context(|| {
            format!(
                "failed to write manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        config.terminal().status(
            "Updated",
            format!("manifest of package `{name}`", name = name.display),
        )?;

        Ok(())
    }

    fn is_command(&self) -> bool {
        self.command || !self.lib
    }

    fn generate_source(
        &self,
        target: &Option<(RegistryResolution, Option<String>)>,
    ) -> Result<Cow<str>> {
        match target {
            Some((resolution, world)) => {
                let generator =
                    SourceGenerator::new(&resolution.id, &resolution.path, !self.no_rustfmt);
                generator.generate(world.as_deref()).map(Into::into)
            }
            None => {
                if self.is_command() {
                    Ok(r#"mod bindings;

fn main() {
    println!("Hello, world!");
}
"#
                    .into())
                } else {
                    Ok(r#"mod bindings;

use bindings::Guest;

struct Component;

impl Guest for Component {
    /// Say hello!
    fn hello_world() -> String {
        "Hello, World!".to_string()
    }
}
"#
                    .into())
                }
            }
        }
    }

    fn create_source_file(
        &self,
        config: &Config,
        out_dir: &Path,
        source: &str,
        target: &Option<(RegistryResolution, Option<String>)>,
    ) -> Result<()> {
        let path = if self.is_command() {
            "src/main.rs"
        } else {
            "src/lib.rs"
        };

        let source_path = out_dir.join(path);
        fs::write(&source_path, source).with_context(|| {
            format!(
                "failed to write source file `{path}`",
                path = source_path.display()
            )
        })?;

        match target {
            Some((resolution, _)) => {
                config.terminal().status(
                    "Generated",
                    format!(
                        "source file `{path}` for target `{id}` v{version}",
                        id = resolution.id,
                        version = resolution.version
                    ),
                )?;
            }
            None => {
                config
                    .terminal()
                    .status("Generated", format!("source file `{path}`"))?;
            }
        }

        Ok(())
    }

    fn create_targets_file(&self, name: &PackageName, out_dir: &Path) -> Result<()> {
        if self.is_command() || self.target.is_some() {
            return Ok(());
        }

        let wit_path = out_dir.join(DEFAULT_WIT_DIR);
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
                r#"package {ns}:{pkg};

/// An example world for the component to target.
world example {{
    export hello-world: func() -> string;
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
    "rust-analyzer.check.overrideCommand": [
        "cargo",
        "component",
        "check",
        "--workspace",
        "--all-targets",
        "--message-format=json"
    ],
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
            Some("emacs") => {
                let settings_path = out_dir.join(".dir-locals.el");

                fs::create_dir_all(out_dir)?;

                fs::write(
                    &settings_path,
                    r#";;; Directory Local Variables
;;; For more information see (info "(emacs) Directory Variables")

((lsp-mode . ((lsp-rust-analyzer-cargo-watch-args . ["check"
                                                     (\, "--message-format=json")])
              (lsp-rust-analyzer-cargo-watch-command . "component")
              (lsp-rust-analyzer-cargo-override-command . ["cargo"
                                                           (\, "component")
                                                           (\, "check")
                                                           (\, "--workspace")
                                                           (\, "--all-targets")
                                                           (\, "--message-format=json")]))))
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
        network_allowed: bool,
    ) -> Result<Option<(RegistryResolution, Option<String>)>> {
        match target {
            Some(metadata::Target::Package { id, package, world }) => {
                let mut resolver = DependencyResolver::new(
                    config.warg(),
                    registries,
                    None,
                    config.terminal(),
                    network_allowed,
                )?;
                let dependency = Dependency::Package(package);

                resolver.add_dependency(&id, &dependency).await?;

                let dependencies = resolver.resolve().await?;
                assert_eq!(dependencies.len(), 1);

                match dependencies
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
