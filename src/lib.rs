//! Cargo support for WebAssembly components.

#![deny(missing_docs)]

use anyhow::{anyhow, bail, Context, Result};
use cargo::{
    core::{Manifest, Package, SourceId, Summary, Workspace},
    ops::{self, CompileOptions, ExportInfo, OutputMetadataOptions},
    util::{interning::InternedString, Filesystem},
    Config,
};
use cargo_util::paths::link_or_copy;
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};
use toml_edit::easy::Value;
use wit_bindgen_core::{Direction, Files, Generator};
use wit_bindgen_gen_guest_rust::Opts;
use wit_component::ComponentEncoder;
use wit_parser::Interface;

mod target;

pub mod commands;

const WIT_BINDGEN_REPO: &str = "https://github.com/bytecodealliance/wit-bindgen";
const COMPONENT_SECTION_PATH: &str = "package.metadata.component";
const IMPORTS_SECTION_PATH: &str = "package.metadata.component.imports";
const EXPORTS_SECTION_PATH: &str = "package.metadata.component.exports";

/// Represents a dependency on a WebAssembly interface file.
#[derive(Debug)]
pub struct InterfaceDependency {
    /// The path to the interface definition file.
    pub path: PathBuf,
    /// The interface definition.
    pub interface: Interface,
}

impl InterfaceDependency {
    fn new(
        config: &Config,
        manifest_dir: &Path,
        name: &str,
        value: &Value,
        section: &str,
    ) -> Result<Self> {
        let path = match value {
            Value::String(s) => {
                // Setting of the form: `dependency = "<path>|<version>"
                // Currently, we assume the value is a path to a wit file
                // In the future, this might be a version number from a registry
                manifest_dir.join(s)
            }
            Value::Table(t) => {
                // Setting is of the form: `<name> = { ...}`
                let mut path = None;

                for (k, v) in t {
                    match k.as_str() {
                        "path" => {
                            path = Some(manifest_dir.join(v.as_str().ok_or_else(|| {
                                    anyhow!("expected a string for `path` of dependency `{name}` in section `{section}`")
                                })?));
                        }
                        k => config.shell().warn(format!(
                            "unsupported key `{k}` in reference `{name}` in section `{section}`"
                        ))?,
                    }
                }

                path.ok_or_else(|| {
                    anyhow!(
                        "setting `path` is missing for dependency `{name}` in section `{section}`"
                    )
                })?
            }
            _ => bail!("expected a string or table for dependency `{name}` in section `{section}`"),
        };

        let mut interface = Interface::parse_file(&path).with_context(|| {
            format!(
                "failed to parse interface file `{path}` for dependency `{name}`",
                path = path.display()
            )
        })?;

        interface.module = Some(name.to_string());
        interface.name = name.to_string();

        Ok(Self { path, interface })
    }
}

/// Represents cargo metadata for a WebAssembly component.
#[derive(Debug)]
pub struct ComponentMetadata {
    /// The package name of the component.
    pub name: String,
    /// The last modified time of the component metadata.
    pub last_modified: SystemTime,
    /// The directly exported interface for the component.
    pub direct_export: Option<InterfaceDependency>,
    /// The import dependencies for the component.
    pub imports: Vec<InterfaceDependency>,
    /// The export dependencies for the component.
    pub exports: Vec<InterfaceDependency>,
}

impl ComponentMetadata {
    /// Creates a new component metadata for the given package.
    ///
    /// Returns `Ok(None)` if the package does not have a component metadata section.
    pub fn from_package(config: &Config, package: &Package) -> Result<Option<Self>> {
        let manifest_path = package.manifest_path();
        let last_modified = last_modified_time(manifest_path)?;
        let manifest_dir = manifest_path.parent().unwrap();

        log::debug!(
            "searching for component metadata in manifest `{path}`",
            path = manifest_path.display()
        );

        let mut names: HashSet<InternedString> = package
            .manifest()
            .dependencies()
            .iter()
            .map(cargo::core::Dependency::name_in_toml)
            .collect();

        let metadata = match package.manifest().custom_metadata() {
            Some(metadata) => metadata,
            None => return Ok(None),
        };

        let component = match metadata.get("component") {
            Some(component) => component,
            None => return Ok(None),
        };

        let imports = Self::read_dependencies(
            manifest_path,
            config,
            manifest_dir,
            &mut names,
            component,
            "imports",
            IMPORTS_SECTION_PATH,
        )?;
        let mut exports = Self::read_dependencies(
            manifest_path,
            config,
            manifest_dir,
            &mut names,
            component,
            "exports",
            EXPORTS_SECTION_PATH,
        )?;

        let direct_export = match component.get("direct-interface-export") {
            Some(v) => {
                let name = v.as_str().ok_or_else(|| {
                    anyhow!("expected a string for `direct-interface-export` in section `{COMPONENT_SECTION_PATH}`")
                })?;

                let index = exports.iter().position(|e| e.interface.name == name).ok_or_else(|| {
                    anyhow!("direct interface export `{name}` does not exist in section `{EXPORTS_SECTION_PATH}`")
                })?;

                // Remove the direct interface from the exports list and clear its module name as
                // it will be exported from the component itself
                let mut export = exports.swap_remove(index);
                export.interface.module = None;

                Some(export)
            }
            None => None,
        };

        Ok(Some(Self {
            name: package.name().to_string(),
            last_modified,
            direct_export,
            imports,
            exports,
        }))
    }

    fn read_dependencies(
        manifest_path: &Path,
        config: &Config,
        manifest_dir: &Path,
        names: &mut HashSet<InternedString>,
        metadata: &Value,
        name: &str,
        section: &str,
    ) -> Result<Vec<InterfaceDependency>> {
        match metadata.get(name) {
            Some(v) => {
                let dependencies = v.as_table().ok_or_else(|| {
                    anyhow!(
                        "section `{section}` manifest `{path}` is required to be a table",
                        path = manifest_path.display()
                    )
                })?;

                let mut interfaces = Vec::with_capacity(dependencies.len());
                for (k, v) in dependencies {
                    if !names.insert(InternedString::new(k)) {
                        bail!("duplicate dependency named `{k}` in section `{section}`");
                    }

                    let interface = InterfaceDependency::new(config, manifest_dir, k, v, section)?;

                    log::debug!(
                        "found interface dependency `{path}`",
                        path = interface.path.display(),
                    );

                    interfaces.push(interface);
                }

                Ok(interfaces)
            }
            None => Ok(Vec::new()),
        }
    }
}

fn update_dependencies(
    config: &Config,
    manifest: &mut Manifest,
    dependencies: Vec<cargo::core::Dependency>,
) -> Result<()> {
    let dependencies = manifest
        .dependencies()
        .iter()
        .cloned()
        .chain(dependencies)
        .collect();

    *manifest.summary_mut() = Summary::new(
        config,
        manifest.package_id(),
        dependencies,
        manifest.original().features().unwrap_or(&BTreeMap::new()),
        manifest.links(),
    )?;

    Ok(())
}

fn last_modified_time(path: impl AsRef<Path>) -> Result<SystemTime> {
    let path = path.as_ref();
    path.metadata()
        .with_context(|| {
            format!(
                "failed to read metadata for `{path}`",
                path = path.display()
            )
        })?
        .modified()
        .with_context(|| {
            format!(
                "failed to retrieve last modified time for `{path}`",
                path = path.display()
            )
        })
}

fn generate_workspace_bindings(
    config: &Config,
    workspace: &mut Workspace,
    force_generation: bool,
) -> Result<Vec<ComponentMetadata>> {
    let mut metadata = Vec::new();
    let bindgen_dir = workspace.target_dir().join("bindgen");

    for package in workspace.members_mut() {
        let component_metadata = match ComponentMetadata::from_package(config, package)? {
            Some(metadata) => metadata,
            None => continue,
        };
        let dependencies =
            generate_dependencies(config, &bindgen_dir, &component_metadata, force_generation)?;
        update_dependencies(config, package.manifest_mut(), dependencies)?;
        metadata.push(component_metadata);
    }

    Ok(metadata)
}

fn generate_dependencies(
    config: &Config,
    target_dir: &Filesystem,
    metadata: &ComponentMetadata,
    force_generation: bool,
) -> Result<Vec<cargo::core::Dependency>> {
    target_dir.create_dir()?;

    let _lock = target_dir.open_rw(".lock", config, "bindings cache")?;
    let target_path = target_dir.as_path_unlocked();
    let last_modified_exe = last_modified_time(std::env::current_exe()?)?;
    let pkg_name = &metadata.name;

    metadata
        .direct_export
        .iter()
        .map(|i| (Direction::Export, i))
        .chain(metadata.imports.iter().map(|i| (Direction::Import, i)))
        .chain(metadata.exports.iter().map(|i| (Direction::Export, i)))
        .map(|(dir, dep)| {
            generate_dependency(
                config,
                pkg_name,
                dep,
                dir,
                target_path,
                metadata.last_modified,
                last_modified_exe,
                force_generation,
            )
        })
        .collect::<Result<_>>()
}

fn is_wasm_module(path: impl AsRef<Path>) -> Result<bool> {
    let path = path.as_ref();

    let mut file = File::open(path)
        .with_context(|| format!("failed to open `{path}` for read", path = path.display()))?;

    let mut bytes = [0u8; 8];
    file.read(&mut bytes).with_context(|| {
        format!(
            "failed to read file header for `{path}`",
            path = path.display()
        )
    })?;

    if bytes[0..4] != [0x0, b'a', b's', b'm'] {
        bail!(
            "expected `{path}` to be a WebAssembly module",
            path = path.display()
        );
    }

    // Check for the module header version
    Ok(bytes[4..] == [0x01, 0x00, 0x00, 0x00])
}

#[allow(clippy::too_many_arguments)]
fn generate_dependency(
    config: &Config,
    pkg_name: &str,
    dependency: &InterfaceDependency,
    dir: Direction,
    target_dir: &Path,
    last_modified_manifest: SystemTime,
    last_modified_exe: SystemTime,
    force_generation: bool,
) -> Result<cargo::core::Dependency> {
    // TODO: when sourcing dependencies from a registry, use actual version information.
    let version = "0.1.0";
    let name = format!("{pkg_name}-{name}", name = &dependency.interface.name);
    let path = &dependency.path;

    let package_dir = target_dir.join(&name);

    fs::create_dir_all(&package_dir).with_context(|| {
        format!(
            "failed to create package directory `{path}`",
            path = package_dir.display()
        )
    })?;

    let manifest_path = package_dir.join("Cargo.toml");
    let source_dir = package_dir.join("src");
    let source_path = source_dir.join("lib.rs");

    let last_modified_input = last_modified_time(path)?;
    let last_modified_output = source_path
        .is_file()
        .then(|| last_modified_time(&source_path))
        .transpose()?
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let manifest_modified = last_modified_manifest > last_modified_output;
    let input_modified = last_modified_input > last_modified_output;
    let exe_modified = last_modified_exe > last_modified_output;

    if force_generation || manifest_modified || input_modified || exe_modified {
        log::debug!(
            "generating dependency `{name}` at `{path}` because {reason}",
            path = package_dir.display(),
            reason = if force_generation {
                "generation was forced"
            } else if manifest_modified {
                "the manifest was modified"
            } else if input_modified {
                "the input file was modified"
            } else if exe_modified {
                "the cargo-component executable was modified"
            } else {
                "of an unknown reason"
            }
        );

        config.shell().status(
            "Generating",
            format!("{name} v{version} ({path})", path = package_dir.display()),
        )?;

        fs::write(
            &manifest_path,
            format!(
                r#"[package]
name = "{name}"
version = "{version}"
edition = "2021"

[dependencies]
"wit-bindgen-guest-rust" = {{ git = "{WIT_BINDGEN_REPO}", default_features = false }}
"#
            ),
        )
        .with_context(|| {
            format!(
                "failed to create manifest `{path}`",
                path = manifest_path.display()
            )
        })?;

        fs::create_dir_all(&source_dir).with_context(|| {
            format!(
                "failed to create source directory `{path}`",
                path = source_dir.display()
            )
        })?;

        let opts = Opts {
            rustfmt: true,
            standalone: true,
            ..Default::default()
        };

        let mut generator = opts.build();
        let mut files = Files::default();
        generator.generate_one(&dependency.interface, dir, &mut files);

        fs::write(
            &source_path,
            files.iter().map(|(_, bytes)| bytes).next().unwrap(),
        )
        .with_context(|| {
            format!(
                "failed to create source file `{path}`",
                path = source_path.display()
            )
        })?;
    } else {
        log::debug!(
            "dependency `{name}` ({version}) at `{path}` is up-to-date",
            path = package_dir.display()
        );
    }

    let mut dep =
        cargo::core::Dependency::parse(name, Some(version), SourceId::for_path(&package_dir)?)?;

    // Set the explicit name in toml to the name of the interface.
    // Without this, dependencies might conflict between two different components
    // in the workspace.
    dep.set_explicit_name_in_toml(&dependency.interface.name);
    Ok(dep)
}

fn create_component(
    config: &Config,
    target_path: &Path,
    metadata: ComponentMetadata,
) -> Result<()> {
    fn to_interface(mut dep: InterfaceDependency) -> Interface {
        if let Some(module) = dep.interface.module.take() {
            dep.interface.name = module;
        }

        dep.interface
    }

    let dep_path = target_path
        .parent()
        .unwrap()
        .join("deps")
        .join(target_path.file_name().unwrap());

    log::debug!(
        "compilation output is `{dep_path}` with target `{target_path}`",
        dep_path = dep_path.display(),
        target_path = target_path.display()
    );

    // If the compilation output is not a WebAssembly module, then no need to generate a component
    if !is_wasm_module(&dep_path)? {
        log::debug!(
            "output file `{path}` is already a WebAssembly component",
            path = dep_path.display()
        );
        return Ok(());
    }

    let ComponentMetadata {
        direct_export,
        imports,
        exports,
        ..
    } = metadata;

    let direct_export = direct_export.map(to_interface);
    let imports: Vec<_> = imports.into_iter().map(to_interface).collect();
    let exports: Vec<_> = exports.into_iter().map(to_interface).collect();
    let module = fs::read(&dep_path).with_context(|| {
        anyhow!(
            "failed to read output module `{path}`",
            path = dep_path.display()
        )
    })?;

    let mut encoder = ComponentEncoder::default()
        .module(&module)
        .imports(&imports)
        .exports(&exports)
        .validate(true);

    if let Some(direct_export) = &direct_export {
        encoder = encoder.interface(direct_export);
    }

    fs::write(&dep_path, encoder.encode()?).with_context(|| {
        anyhow!(
            "failed to write output component `{path}`",
            path = dep_path.display()
        )
    })?;

    config.shell().status(
        "Creating",
        format!("component {path}", path = target_path.display()),
    )?;

    // Finally, link the dep path to the target path to create the final target
    link_or_copy(dep_path, target_path)
}

/// Compile a component for the given workspace and compile options.
///
/// It is expected that the current package contains a `package.metadata.component` section.
pub fn compile(
    config: &Config,
    mut workspace: Workspace,
    options: &CompileOptions,
    force_generation: bool,
) -> Result<()> {
    let metadata = generate_workspace_bindings(config, &mut workspace, force_generation)?;
    let result = ops::compile(&workspace, options)?;

    for m in metadata {
        let path = result
            .cdylibs
            .iter()
            .find(|o| o.unit.pkg.name() == m.name.as_str())
            .map(|o| &o.path)
            .ok_or_else(|| {
                anyhow!(
                    "failed to find output for component package `{}`",
                    m.name.as_str()
                )
            })?;

        create_component(config, path, m)?;
    }

    Ok(())
}

/// Retrieves workspace metadata for the given workspace and metadata options.
///
/// The returned metadata contains information about generated dependencies.
pub fn metadata(
    config: &Config,
    mut workspace: Workspace,
    options: &OutputMetadataOptions,
) -> Result<ExportInfo> {
    generate_workspace_bindings(config, &mut workspace, false)?;
    ops::output_metadata(&workspace, options)
}

/// Check a component for errors with the given workspace and compile options.
pub fn check(
    config: &Config,
    mut workspace: Workspace,
    options: &CompileOptions,
    force_generation: bool,
) -> Result<()> {
    generate_workspace_bindings(config, &mut workspace, force_generation)?;
    ops::compile(&workspace, options)?;
    Ok(())
}
