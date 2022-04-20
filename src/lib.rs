//! Cargo support for WebAssembly components.

#![deny(missing_docs)]

use anyhow::{anyhow, bail, Context, Result};
use cargo::{
    core::{compiler::Compilation, Manifest, Package, SourceId, Summary, Workspace},
    ops::{self, CompileOptions, ExportInfo, OutputMetadataOptions},
    util::interning::InternedString,
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
use wit_bindgen_gen_core::{Direction, Files, Generator};
use wit_bindgen_gen_rust_wasm::Opts;
use wit_component::ComponentEncoder;
use wit_parser::Interface;

mod target;

pub mod commands;

const COMPONENT_PATH: &str = "package.metadata.component";
const DEPENDENCIES_PATH: &str = "package.metadata.component.dependencies";

/// Represents a dependency on a WebAssembly interface file.
#[derive(Debug)]
pub struct InterfaceDependency {
    /// The version string for the interface.
    pub version: String,
    /// The path to the interface definition file.
    pub path: PathBuf,
    /// The interface definition.
    pub interface: Interface,
}

enum Dependency {
    Default { path: PathBuf, interface: Interface },
    Export(InterfaceDependency),
    Import(InterfaceDependency),
}

impl Dependency {
    fn new(config: &Config, dir: &Path, name: &str, value: &Value) -> Result<Self> {
        match value {
            Value::String(s) => {
                // Setting is of the form "<name>" = "<version>"
                // TODO: remove this in the future when version references (i.e. registry
                // references) are supported.
                bail!(
                    "referencing `{}` by version `{}` is not yet supported in section `{}`",
                    name,
                    s,
                    DEPENDENCIES_PATH,
                )
            }
            Value::Table(t) => {
                // Setting is of the form `<name> = { ...}`
                let mut version = None;
                let mut path = None;
                let mut export = None;
                let mut interface = None;

                for (k, v) in t {
                    match k.as_str() {
                        "version" => {
                            version = Some(v.as_str().ok_or_else(|| {
                                anyhow!(
                                    "expected a string for `version` of dependency `{}` in section `{}`",
                                    name,
                                    DEPENDENCIES_PATH
                                )
                            })?);
                        }
                        "path" => {
                            let p = dir.join(v.as_str().ok_or_else(|| {
                                    anyhow!(
                                        "expected a string for `path` of dependency `{}` in section `{}`",
                                        name,
                                        DEPENDENCIES_PATH
                                    )
                                })?);

                            let i = Interface::parse_file(&p).with_context(|| {
                                format!(
                                    "failed to parse interface file `{}` for dependency `{}`",
                                    p.display(),
                                    name
                                )
                            })?;

                            path = Some(p);
                            interface = Some(i);
                        }
                        "export" => {
                            export = Some(v.as_bool().ok_or_else(|| {
                                anyhow!(
                                    "expected a boolean for `export` of dependency `{}` in section `{}`",
                                    name,
                                    DEPENDENCIES_PATH
                                )
                            })?);
                        }
                        k => config.shell().warn(format!(
                            "unsupported key `{}` in reference `{}` in section `{}`",
                            k, name, DEPENDENCIES_PATH
                        ))?,
                    }
                }

                match (version, path, export) {
                    (Some(version), Some(path), export) => {
                        let mut interface = interface.unwrap();
                        interface.module = Some(format!("{name}-{version}", name = interface.name));
                        interface.name = name.to_string();

                        let file = InterfaceDependency {
                            version: version.to_string(),
                            path,
                            interface,
                        };

                        if let Some(true) = export {
                            Ok(Self::Export(file))
                        } else {
                            Ok(Self::Import(file))
                        }
                    }
                    (None, Some(path), Some(true)) => {
                        let mut interface = interface.unwrap();
                        interface.name = name.to_string();

                        Ok(Self::Default { path, interface })
                    }
                    (None, _, _) => bail!(
                        "setting `version` is missing for dependency `{}` in section `{}`",
                        name,
                        DEPENDENCIES_PATH,
                    ),
                    (_, None, _) => bail!(
                        "setting `path` is missing for dependency `{}` in section `{}`",
                        name,
                        DEPENDENCIES_PATH,
                    ),
                }
            }
            _ => bail!(
                "expected a string or table for dependency `{}` in section `{}`",
                name,
                DEPENDENCIES_PATH
            ),
        }
    }
}

/// Represents cargo metadata for a WebAssembly component.
#[derive(Debug)]
pub struct ComponentMetadata {
    /// The last modified time of the component metadata.
    pub last_modified: SystemTime,
    /// The default interface for the component.
    pub interface: Option<InterfaceDependency>,
    /// The import dependencies for the component.
    pub imports: Vec<InterfaceDependency>,
    /// The export dependencies for the component.
    pub exports: Vec<InterfaceDependency>,
}

impl ComponentMetadata {
    /// Creates a new component metadata for the given package.
    pub fn from_package(config: &Config, package: &Package) -> Result<Self> {
        let path = package.manifest_path();
        let last_modified = last_modified_time(path)?;
        let dir = path.parent().unwrap();

        log::debug!(
            "searching for component metadata in manifest `{path}`",
            path = path.display()
        );

        let mut names: HashSet<InternedString> = package
            .manifest()
            .dependencies()
            .iter()
            .map(cargo::core::Dependency::name_in_toml)
            .collect();

        let metadata = package.manifest().custom_metadata().ok_or_else(|| {
            anyhow!(
                "manifest `{}` does not contain a `{}` section",
                path.display(),
                COMPONENT_PATH,
            )
        })?;

        let component = metadata.get("component").ok_or_else(|| {
            anyhow!(
                "manifest `{}` does not contain a `{}` section",
                path.display(),
                COMPONENT_PATH,
            )
        })?;

        let mut interface = None;
        let mut imports = Vec::new();
        let mut exports = Vec::new();
        if let Some(dependencies) = component.get("dependencies") {
            let dependencies = dependencies.as_table().ok_or_else(|| {
                anyhow!(
                    "setting `{}` in manifest `{}` is required to be a table",
                    DEPENDENCIES_PATH,
                    path.display()
                )
            })?;

            for (k, v) in dependencies {
                if !names.insert(InternedString::new(k)) {
                    bail!(
                        "duplicate dependency named `{}` in section `{}`",
                        k,
                        DEPENDENCIES_PATH
                    );
                }

                match Dependency::new(config, dir, k, v)? {
                    Dependency::Default { path, interface: i } => {
                        log::debug!(
                            "found default interface dependency `{path}`",
                            path = path.display()
                        );
                        if interface.is_some() {
                            bail!(
                                "a default interface cannot be specified more than once in section `{}`",
                                DEPENDENCIES_PATH
                            );
                        }

                        interface = Some(InterfaceDependency {
                            version: package.version().to_string(),
                            path,
                            interface: i,
                        });
                    }
                    Dependency::Export(i) => {
                        log::debug!(
                            "found export interface dependency `{path}` ({version})",
                            path = i.path.display(),
                            version = i.version
                        );
                        exports.push(i);
                    }
                    Dependency::Import(i) => {
                        log::debug!(
                            "found import interface dependency `{path}` ({version})",
                            path = i.path.display(),
                            version = i.version
                        );
                        imports.push(i);
                    }
                }
            }
        }

        Ok(Self {
            last_modified,
            interface,
            imports,
            exports,
        })
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
        .with_context(|| format!("failed to read metadata for `{}`", path.display()))?
        .modified()
        .with_context(|| {
            format!(
                "failed to retrieve last modified time for `{}`",
                path.display()
            )
        })
}

/// Generates dependency crates for the given component metadata.
///
/// This function is responsible for generating the bindings for the component's imports
/// and exports before a compilation step.
pub fn generate_dependencies(
    config: &Config,
    workspace: &mut Workspace,
    metadata: &ComponentMetadata,
    force_generation: bool,
) -> Result<Vec<cargo::core::Dependency>> {
    let target_dir = workspace.target_dir().join("bindgen");
    target_dir.create_dir()?;

    let _lock = target_dir.open_rw(".lock", config, "bindings cache")?;
    let target_path = target_dir.as_path_unlocked();
    let last_modified_exe = last_modified_time(std::env::current_exe()?)?;

    metadata
        .interface
        .iter()
        .map(|i| (Direction::Export, i))
        .chain(metadata.imports.iter().map(|i| (Direction::Import, i)))
        .chain(metadata.exports.iter().map(|i| (Direction::Export, i)))
        .map(|(dir, dep)| {
            generate_dependency(
                config,
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
        .with_context(|| format!("failed to open `{}` for read", path.display()))?;

    let mut bytes = [0u8; 8];
    file.read(&mut bytes)
        .with_context(|| format!("failed to read file header for `{}`", path.display()))?;

    if bytes[0..4] != [0x0, b'a', b's', b'm'] {
        bail!("expected `{}` to be a WebAssembly module", path.display());
    }

    // Check for the module header version
    Ok(bytes[4..] == [0x01, 0x00, 0x00, 0x00])
}

fn generate_dependency(
    config: &Config,
    dependency: &InterfaceDependency,
    dir: Direction,
    target_dir: &Path,
    last_modified_manifest: SystemTime,
    last_modified_exe: SystemTime,
    force_generation: bool,
) -> Result<cargo::core::Dependency> {
    let name = &dependency.interface.name;
    let version = &dependency.version;
    let path = &dependency.path;

    let package_dir = target_dir.join(name);

    fs::create_dir_all(&package_dir).with_context(|| {
        format!(
            "failed to create package directory `{}`",
            package_dir.display()
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
            "generating dependency `{name}` ({version}) at `{path}` because {reason}",
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
                "\
              [package]\n\
              name = \"{name}\"\n\
              version = \"{version}\"\n\
              edition = \"2021\"\n\
              "
            ),
        )
        .with_context(|| format!("failed to create manifest `{}`", manifest_path.display()))?;

        fs::create_dir_all(&source_dir).with_context(|| {
            format!(
                "failed to create source directory `{}`",
                source_dir.display()
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
        .with_context(|| format!("failed to create source file `{}`", source_path.display()))?;
    } else {
        log::debug!(
            "dependency `{name}` ({version}) at `{path}` is up-to-date",
            path = package_dir.display()
        );
    }

    cargo::core::Dependency::parse(name, Some(version), SourceId::for_path(&package_dir)?)
}

fn create_component(
    config: &Config,
    result: Compilation,
    metadata: ComponentMetadata,
) -> Result<()> {
    fn to_interface(mut dep: InterfaceDependency) -> Interface {
        if let Some(module) = dep.interface.module.take() {
            dep.interface.name = module;
        }

        dep.interface
    }

    if result.cdylibs.len() != 1 {
        bail!("expected compilation output to be a single cdylib");
    }

    let target_path = &result.cdylibs[0].path;
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
        interface,
        imports,
        exports,
        ..
    } = metadata;

    let interface = interface.map(to_interface);
    let imports: Vec<_> = imports.into_iter().map(to_interface).collect();
    let exports: Vec<_> = exports.into_iter().map(to_interface).collect();
    let module = fs::read(&dep_path)
        .with_context(|| anyhow!("failed to read output module `{}`", dep_path.display()))?;

    let mut encoder = ComponentEncoder::default()
        .module(&module)
        .imports(&imports)
        .exports(&exports)
        .validate(true);

    if let Some(interface) = &interface {
        encoder = encoder.interface(interface);
    }

    fs::write(&dep_path, encoder.encode()?)
        .with_context(|| anyhow!("failed to write output component `{}`", dep_path.display()))?;

    config
        .shell()
        .status("Creating", format!("component {}", target_path.display()))?;

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
    let metadata = ComponentMetadata::from_package(config, workspace.current()?)?;
    let dependencies = generate_dependencies(config, &mut workspace, &metadata, force_generation)?;

    update_dependencies(
        config,
        workspace.current_mut()?.manifest_mut(),
        dependencies,
    )?;

    let result = ops::compile(&workspace, options)?;
    create_component(config, result, metadata)?;

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
    let component_metadata = ComponentMetadata::from_package(config, workspace.current()?)?;
    let dependencies = generate_dependencies(config, &mut workspace, &component_metadata, false)?;

    update_dependencies(
        config,
        workspace.current_mut()?.manifest_mut(),
        dependencies,
    )?;

    ops::output_metadata(&workspace, options)
}

/// Check a component for errors with the given workspace and compile options.
///
/// It is expected that the current package contains a `package.metadata.component` section.
pub fn check(
    config: &Config,
    mut workspace: Workspace,
    options: &CompileOptions,
    force_generation: bool,
) -> Result<()> {
    let metadata = ComponentMetadata::from_package(config, workspace.current()?)?;
    let dependencies = generate_dependencies(config, &mut workspace, &metadata, force_generation)?;

    update_dependencies(
        config,
        workspace.current_mut()?.manifest_mut(),
        dependencies,
    )?;

    ops::compile(&workspace, options)?;
    Ok(())
}
