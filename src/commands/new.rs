use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{bail, ensure, Context, Result};
use cargo_component_core::{
    command::CommonOptions,
    registry::{Dependency, DependencyResolution, DependencyResolver, RegistryResolution},
};
use clap::Args;
use heck::ToKebabCase;
use semver::VersionReq;
use toml_edit::{table, value, DocumentMut, Item, Table, Value};
use wasm_pkg_client::caching::{CachingClient, FileCache};
use wasm_pkg_client::{CustomConfig, PackageRef, Registry, RegistryMapping, RegistryMetadata};

use crate::config::Config;
use crate::generator::SourceGenerator;
use crate::metadata::DEFAULT_WIT_DIR;
use crate::{generate_bindings, load_component_metadata, load_metadata, metadata, CargoArguments};

const WIT_BINDGEN_RT_CRATE: &str = "wit-bindgen-rt";

/// Name of a given package
struct PackageName<'a> {
    /// Namespace of the package
    namespace: String,

    /// Name of the package
    name: String,

    /// Value that should be used when displaying the package name
    display: Cow<'a, str>,
}

impl<'a> PackageName<'a> {
    /// Create a new package name
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
        ensure!(
            !namespace_kebab.is_empty(),
            "invalid component namespace `{namespace}`"
        );

        wit_parser::validate_id(&namespace_kebab).with_context(|| {
            format!("component namespace `{namespace}` is not a legal WIT identifier")
        })?;

        let name_kebab = name.to_kebab_case();
        ensure!(!name_kebab.is_empty(), "invalid component name `{name}`");

        wit_parser::validate_id(&name_kebab)
            .with_context(|| format!("component name `{name}` is not a legal WIT identifier"))?;

        Ok(Self {
            namespace: namespace_kebab,
            name: name_kebab,
            display,
        })
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
    #[clap(long = "bin", alias = "command", conflicts_with = "lib")]
    pub bin: bool,

    /// Create a library (reactor) component
    #[clap(long = "lib", alias = "reactor")]
    pub lib: bool,

    /// Use the built-in `wasi:http/proxy` module adapter
    #[clap(long = "proxy", requires = "lib")]
    pub proxy: bool,

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

    /// Registry to use as the default when generating the package
    ///
    /// (e.g. 'oci://ghcr.io')
    /// NOTE: you may need to also specify --registry-ns-prefix
    #[clap(long = "registry", value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// Namespace prefix to use with the custom registry provided,
    /// most commonly used with an OCI registry (e.g. 'oci://ghcr.io')
    ///
    /// (e.g. 'bytecodealliance/')
    #[clap(long = "registry-ns-prefix", value_name = "REGISTRY_NS_PREFIX")]
    pub registry_ns_prefix: Option<String>,

    /// Disable the use of `rustfmt` when generating source code.
    #[clap(long = "no-rustfmt")]
    pub no_rustfmt: bool,

    /// The path for the generated package.
    #[clap(value_name = "path")]
    pub path: PathBuf,
}

impl NewCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing new command");

        // Build configuration
        let mut config =
            Config::new(self.common.new_terminal(), self.common.config.clone()).await?;

        // Support OCI registries when resolving target worlds
        match (self.target.as_ref(), self.registry.as_ref()) {
            // Support specifying OCI registries with
            (Some(target), Some(oci_uri)) if oci_uri.starts_with("oci://") => {
                // Build registry & mapping configuration
                let raw_registry = oci_uri.split_at(6).1;
                let registry =
                    Registry::from_str(raw_registry).context("parsing provided registry")?;

                // Configure OCI metadata
                let mut metadata = RegistryMetadata::default();
                metadata.preferred_protocol = Some("oci".into());
                metadata.set_oci_registry(Some(registry.clone().into()));
                if let Some(ref raw_ns_prefix) = self.registry_ns_prefix {
                    // Ensure prefix, if provided, ends with '/'
                    let ns_prefix = if raw_ns_prefix.is_empty() || raw_ns_prefix.ends_with("/") {
                        raw_ns_prefix.into()
                    } else {
                        format!("{raw_ns_prefix}/")
                    };
                    metadata.set_oci_namespace_prefix(Some(ns_prefix))
                }

                // Build registry mapping
                log::debug!(
                    "using namespace registry [{raw_registry}] {} for target [{target}]",
                    self.registry_ns_prefix
                        .as_ref()
                        .map(|v| format!("(prefix {}", v))
                        .unwrap_or_default(),
                );
                let registry_mapping = RegistryMapping::Custom(CustomConfig { registry, metadata });

                // Create an override for the given target package
                config.pkg_config.set_package_registry_override(
                    PackageRef::from_str(target)
                        .with_context(|| format!("converting [{target}] to package ref"))?,
                    registry_mapping,
                );
            }
            // Ignore other cases
            _ => {}
        }

        let name = PackageName::new(&self.namespace, self.name.as_deref(), &self.path)?;

        let out_dir = std::env::current_dir()
            .with_context(|| "couldn't get the current directory of the process")?
            .join(&self.path);

        let target: Option<metadata::Target> = match self.target.as_deref() {
            Some(s) if s.contains('@') => Some(s.parse()?),
            Some(s) => Some(format!("{s}@{version}", version = VersionReq::STAR).parse()?),
            None => None,
        };
        let client = config
            .client(self.common.cache_dir.clone(), false)
            .await
            .context("building client")?;

        let target = self
            .resolve_target(Arc::clone(&client), target)
            .await
            .context("resolving target world")?;
        let source = self
            .generate_source(&target)
            .await
            .context("generating source code")?;

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

        let target = target.map(|(res, world)| {
            match res {
                DependencyResolution::Registry(reg) => (reg, world),
                // This is unreachable because when we got the initial target, we made sure it was a
                // registry target.
                _ => unreachable!(),
            }
        });
        self.update_manifest(&config, &name, &out_dir, &target)?;
        self.create_source_file(&config, &out_dir, source.as_ref(), &target)?;
        self.create_targets_file(&name, &out_dir)?;
        self.create_editor_settings_file(&out_dir)?;

        // Now that we've created the project, generate the bindings so that
        // users can start looking at code with an IDE and not see red squiggles.
        let cargo_args = CargoArguments::parse()?;
        let manifest_path = out_dir.join("Cargo.toml");
        let metadata = load_metadata(Some(&manifest_path))?;
        let packages =
            load_component_metadata(&metadata, cargo_args.packages.iter(), cargo_args.workspace)?;
        let _import_name_map =
            generate_bindings(client, &config, &metadata, &packages, &cargo_args).await?;

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
        target: &Option<(RegistryResolution, Option<String>)>,
    ) -> Result<()> {
        let manifest_path = out_dir.join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path).with_context(|| {
            format!(
                "failed to read manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        let mut doc: DocumentMut = manifest.parse().with_context(|| {
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
                // if specifying exact version, set that exact version in the Cargo.toml
                let version = if !resolution.requirement.comparators.is_empty()
                    && resolution.requirement.comparators[0].op == semver::Op::Exact
                {
                    format!("={}", resolution.version)
                } else {
                    format!("{}", resolution.version)
                };
                component["target"] = match world {
                    Some(world) => {
                        value(format!("{name}/{world}@{version}", name = resolution.name,))
                    }
                    None => value(format!("{name}@{version}", name = resolution.name,)),
                };
            }
        }

        component["dependencies"] = Item::Table(Table::new());

        if self.proxy {
            component["proxy"] = value(true);
        }

        let mut metadata = Table::new();
        metadata.set_implicit(true);
        metadata.set_position(doc.len());
        metadata["component"] = Item::Table(component);
        doc["package"]["metadata"] = Item::Table(metadata);

        fs::write(&manifest_path, doc.to_string()).with_context(|| {
            format!(
                "failed to write manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        // Run cargo add for wit-bindgen and bitflags
        let mut cargo_add_command = std::process::Command::new("cargo");
        cargo_add_command.arg("add");
        cargo_add_command.arg("--quiet");
        cargo_add_command.arg(WIT_BINDGEN_RT_CRATE);
        cargo_add_command.arg("--features");
        cargo_add_command.arg("bitflags");
        cargo_add_command.current_dir(out_dir);
        let status = cargo_add_command
            .status()
            .context("failed to execute `cargo add` command")?;
        if !status.success() {
            bail!("`cargo add {WIT_BINDGEN_RT_CRATE} --features bitflags` command exited with non-zero status");
        }

        config.terminal().status(
            "Updated",
            format!("manifest of package `{name}`", name = name.display),
        )?;

        Ok(())
    }

    fn is_command(&self) -> bool {
        self.bin || !self.lib
    }

    async fn generate_source(
        &self,
        target: &Option<(DependencyResolution, Option<String>)>,
    ) -> Result<Cow<str>> {
        match target {
            Some((resolution, world)) => {
                let generator =
                    SourceGenerator::new(resolution, resolution.name(), !self.no_rustfmt);
                generator.generate(world.as_deref()).await.map(Into::into)
            }
            None => {
                if self.is_command() {
                    Ok(r#"fn main() {
    println!("Hello, world!");
}
"#
                    .into())
                } else {
                    Ok(r#"#[allow(warnings)]
mod bindings;

use bindings::Guest;

struct Component;

impl Guest for Component {
    /// Say hello!
    fn hello_world() -> String {
        "Hello, World!".to_string()
    }
}

bindings::export!(Component with_types_in bindings);
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
                        "source file `{path}` for target `{name}` v{version}",
                        name = resolution.name,
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

    /// This will always return a registry resolution if it is `Some`, but we return the
    /// `DependencyResolution` instead so we can actually resolve the dependency.
    async fn resolve_target(
        &self,
        client: Arc<CachingClient<FileCache>>,
        target: Option<metadata::Target>,
    ) -> Result<Option<(DependencyResolution, Option<String>)>> {
        match target {
            Some(metadata::Target::Package {
                name,
                package,
                world,
            }) => {
                let mut resolver = DependencyResolver::new_with_client(client, None)?;
                let dependency = Dependency::Package(package);

                resolver.add_dependency(&name, &dependency).await?;

                let dependencies = resolver.resolve().await?;
                assert_eq!(dependencies.len(), 1);

                Ok(Some((
                    dependencies
                        .into_values()
                        .next()
                        .expect("expected a target resolution"),
                    world,
                )))
            }
            _ => Ok(None),
        }
    }
}

/// Escape an identifier used in WIT, adding the `%` prefix if it's a known identifier
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
