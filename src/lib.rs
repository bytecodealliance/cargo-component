//! Cargo support for WebAssembly components.

#![deny(missing_docs)]

use crate::{config::Config, metadata::ComponentMetadata};
use anyhow::{anyhow, bail, Context, Result};
use bindings::BindingsGenerator;
use cargo::{
    core::{SourceId, Summary, Workspace},
    ops::{self, CompileOptions, ExportInfo, OutputMetadataOptions},
};
use cargo_util::paths::link_or_copy;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::Path,
    time::SystemTime,
};
use wit_component::ComponentEncoder;

pub mod bindings;
pub mod commands;
pub mod config;
pub mod log;
pub mod metadata;
pub mod registry;
mod target;

const WIT_BINDGEN_REPO: &str = "https://github.com/bytecodealliance/wit-bindgen";

fn last_modified_time(path: impl AsRef<Path>) -> Result<SystemTime> {
    let path = path.as_ref();
    path.metadata()
        .with_context(|| {
            format!(
                "failed to read file metadata for `{path}`",
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

async fn generate_workspace_bindings<'cfg>(
    config: &Config,
    workspace: &mut Workspace<'cfg>,
    force_generation: bool,
) -> Result<Vec<ComponentMetadata>> {
    let mut metadata = Vec::new();
    let bindings_dir = workspace.target_dir().join("bindings");
    let _lock = bindings_dir.open_rw(".lock", config.cargo(), "bindings cache")?;
    let last_modified_exe = last_modified_time(std::env::current_exe()?)?;

    for package in workspace.members_mut() {
        let component_metadata = match ComponentMetadata::from_package(package)? {
            Some(metadata) => metadata,
            None => continue,
        };

        let dependency = generate_package_bindings(
            config,
            &component_metadata,
            bindings_dir.as_path_unlocked(),
            last_modified_exe,
            force_generation,
        )
        .await?;

        let manifest = package.manifest_mut();
        let dependencies = manifest
            .dependencies()
            .iter()
            .cloned()
            .chain([dependency])
            .collect();

        *manifest.summary_mut() = Summary::new(
            config.cargo(),
            manifest.package_id(),
            dependencies,
            manifest.original().features().unwrap_or(&BTreeMap::new()),
            manifest.links(),
        )?;

        metadata.push(component_metadata);
    }

    Ok(metadata)
}

async fn generate_package_bindings(
    config: &Config,
    metadata: &ComponentMetadata,
    bindings_dir: &Path,
    last_modified_exe: SystemTime,
    force: bool,
) -> Result<cargo::core::Dependency> {
    let target_dependencies = metadata
        .section
        .target
        .as_ref()
        .map(|t| t.dependencies())
        .unwrap_or_default();

    // Resolve the dependencies of the component
    let dependencies = registry::resolve(
        config,
        &metadata.section.registries,
        target_dependencies.as_ref(),
        &metadata.section.dependencies,
    )
    .await?;

    let generator = BindingsGenerator::new(bindings_dir, metadata, dependencies)?;

    match generator.reason(last_modified_exe, force)? {
        Some(reason) => {
            ::log::debug!(
                "generating bindings package `{name}` at `{path}` because {reason}",
                name = generator.package_name(),
                path = generator.package_dir().display(),
            );

            config.shell().status(
                "Generating",
                format!(
                    "bindings for {name} ({path})",
                    name = metadata.name,
                    path = generator.package_dir().display()
                ),
            )?;

            generator.generate()?;
        }
        None => {
            ::log::debug!(
                "bindings package `{name}` at `{path}` is up-to-date",
                name = generator.package_name(),
                path = generator.package_dir().display()
            );
        }
    }

    let mut dep = cargo::core::Dependency::parse(
        generator.package_name(),
        Some(&metadata.version.to_string()),
        SourceId::for_path(generator.package_dir())?,
    )?;

    // Set the explicit name in toml to the crate name expected in user source
    dep.set_explicit_name_in_toml("bindings");
    Ok(dep)
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

fn create_component(config: &Config, target_path: &Path) -> Result<()> {
    let dep_path = target_path
        .parent()
        .unwrap()
        .join("deps")
        .join(target_path.file_name().unwrap());

    ::log::debug!(
        "compilation output is `{dep_path}` with target `{target_path}`",
        dep_path = dep_path.display(),
        target_path = target_path.display()
    );

    // If the compilation output is not a WebAssembly module, then no need to generate a component
    if !is_wasm_module(&dep_path)? {
        ::log::debug!(
            "output file `{path}` is already a WebAssembly component",
            path = dep_path.display()
        );
        return Ok(());
    }

    let module = fs::read(&dep_path).with_context(|| {
        anyhow!(
            "failed to read output module `{path}`",
            path = dep_path.display()
        )
    })?;

    config.shell().status(
        "Creating",
        format!("component {path}", path = target_path.display()),
    )?;

    let encoder = ComponentEncoder::default().module(&module)?.validate(true);

    fs::write(&dep_path, encoder.encode()?).with_context(|| {
        anyhow!(
            "failed to write output component `{path}`",
            path = dep_path.display()
        )
    })?;

    // Finally, link the dep path to the target path to create the final target
    link_or_copy(dep_path, target_path)
}

/// Compile a component for the given workspace and compile options.
///
/// It is expected that the current package contains a `package.metadata.component` section.
pub async fn compile<'cfg>(
    config: &'cfg Config,
    mut workspace: Workspace<'cfg>,
    options: &CompileOptions,
    force_generation: bool,
) -> Result<()> {
    let metadata = generate_workspace_bindings(config, &mut workspace, force_generation).await?;
    let result = ops::compile(&workspace, options)?;

    for m in metadata {
        let path = result
            .cdylibs
            .iter()
            .find(|o| o.unit.pkg.name() == m.name.as_str())
            .map(|o| &o.path)
            .ok_or_else(|| {
                anyhow!(
                    "failed to find output for component package `{package}`",
                    package = m.name.as_str()
                )
            })?;

        create_component(config, path)?;
    }

    Ok(())
}

/// Retrieves workspace metadata for the given workspace and metadata options.
///
/// The returned metadata contains information about generated dependencies.
pub async fn metadata<'cfg>(
    config: &'cfg Config,
    mut workspace: Workspace<'cfg>,
    options: &OutputMetadataOptions,
) -> Result<ExportInfo> {
    generate_workspace_bindings(config, &mut workspace, false).await?;
    ops::output_metadata(&workspace, options)
}

/// Check a component for errors with the given workspace and compile options.
pub async fn check<'cfg>(
    config: &'cfg Config,
    mut workspace: Workspace<'cfg>,
    options: &CompileOptions,
    force_generation: bool,
) -> Result<()> {
    generate_workspace_bindings(config, &mut workspace, force_generation).await?;
    ops::compile(&workspace, options)?;
    Ok(())
}
