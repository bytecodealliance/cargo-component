//! Cargo support for WebAssembly components.

#![deny(missing_docs)]

use crate::config::Config;
use anyhow::{anyhow, bail, Context as _, Result};
use bindings::BindingsGenerator;
use cargo::{
    core::{SourceId, Summary, Workspace},
    ops::{self, CompileOptions, DocOptions, ExportInfo, OutputMetadataOptions, UpdateOptions},
};
use registry::{
    LockFile, LockedPackage, LockedPackageVersion, PackageDependencyResolution,
    PackageResolutionMap,
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::Path,
    time::SystemTime,
};
use termcolor::Color;
use wit_component::ComponentEncoder;

pub mod bindings;
pub mod commands;
pub mod config;
pub mod log;
pub mod metadata;
pub mod registry;
mod target;

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

async fn create_resolution_map(
    config: &Config,
    workspace: &Workspace<'_>,
    lock_file: Option<&LockFile>,
) -> Result<PackageResolutionMap> {
    let mut map = PackageResolutionMap::default();
    for package in workspace.members() {
        match PackageDependencyResolution::new(config, workspace, package, lock_file).await? {
            Some(resolution) => {
                let prev = map.insert(package.package_id(), resolution);
                assert!(prev.is_none());
            }
            None => continue,
        }
    }
    Ok(map)
}

async fn resolve_dependencies(
    config: &Config,
    workspace: &Workspace<'_>,
) -> Result<PackageResolutionMap> {
    let lock_file = LockFile::open(config, workspace)?.unwrap_or_default();
    let map = create_resolution_map(config, workspace, Some(&lock_file)).await?;
    let new_lock_file = LockFile::from_resolution(&map);
    new_lock_file.update(config, workspace, &lock_file)?;

    Ok(map)
}

async fn generate_workspace_bindings(
    config: &Config,
    workspace: &mut Workspace<'_>,
    force_generation: bool,
) -> Result<PackageResolutionMap> {
    let map = resolve_dependencies(config, workspace).await?;
    let bindings_dir = workspace.target_dir().join("bindings");
    let _lock = bindings_dir.open_rw(".lock", config.cargo(), "bindings cache")?;
    let last_modified_exe = last_modified_time(std::env::current_exe()?)?;

    for package in workspace.members_mut() {
        let resolution = match map.get(&package.package_id()) {
            Some(resolution) => resolution,
            None => continue,
        };

        let dependency = generate_package_bindings(
            config,
            resolution,
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
    }

    Ok(map)
}

async fn generate_package_bindings(
    config: &Config,
    resolution: &PackageDependencyResolution,
    bindings_dir: &Path,
    last_modified_exe: SystemTime,
    force: bool,
) -> Result<cargo::core::Dependency> {
    let generator = BindingsGenerator::new(bindings_dir, resolution)?;

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
                    name = resolution.metadata.name,
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
        Some(&resolution.metadata.version.to_string()),
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

fn create_component(config: &Config, path: &Path, binary: bool) -> Result<()> {
    // If the compilation output is not a WebAssembly module, then do nothing
    // Note: due to the way cargo currently works on macOS, it will overwrite
    // a previously generated component on an up-to-date build.
    //
    // As a result, users on macOS will see a "creating component" message
    // even if the build is up-to-date.
    //
    // See: https://github.com/rust-lang/cargo/blob/99ad42deb4b0be0cdb062d333d5e63460a94c33c/crates/cargo-util/src/paths.rs#L542-L550
    if !is_wasm_module(path)? {
        ::log::debug!(
            "output file `{path}` is already a WebAssembly component",
            path = path.display()
        );
        return Ok(());
    }

    ::log::debug!(
        "componentizing WebAssembly module `{path}`",
        path = path.display()
    );

    let module = fs::read(path).with_context(|| {
        anyhow!(
            "failed to read output module `{path}`",
            path = path.display()
        )
    })?;

    config.shell().status(
        "Creating",
        format!("component {path}", path = path.display()),
    )?;

    let encoder = ComponentEncoder::default()
        .adapter(
            "wasi_snapshot_preview1",
            if binary {
                include_bytes!("../adapters/408f0bf/wasi_snapshot_preview1.command.wasm")
            } else {
                include_bytes!("../adapters/408f0bf/wasi_snapshot_preview1.reactor.wasm")
            },
        )?
        .module(&module)?
        .validate(true);

    let mut producers = wasm_metadata::Producers::empty();
    producers.add(
        "processed-by",
        env!("CARGO_PKG_NAME"),
        option_env!("CARGO_VERSION_INFO").unwrap_or(env!("CARGO_PKG_VERSION")),
    );

    let component = producers.add_to_wasm(&encoder.encode()?).with_context(|| {
        anyhow!(
            "failed to add metadata to output component `{path}`",
            path = path.display()
        )
    })?;

    fs::write(path, component).with_context(|| {
        anyhow!(
            "failed to write output component `{path}`",
            path = path.display()
        )
    })
}

/// Compile a component for the given workspace and compile options.
///
/// It is expected that the current package contains a `package.metadata.component` section.
pub async fn compile(
    config: &Config,
    mut workspace: Workspace<'_>,
    options: &CompileOptions,
    force_generation: bool,
) -> Result<()> {
    let map = generate_workspace_bindings(config, &mut workspace, force_generation).await?;
    let compilation = ops::compile(&workspace, options)?;

    for (binary, output) in compilation
        .binaries
        .iter()
        .map(|o| (true, o))
        .chain(compilation.cdylibs.iter().map(|o| (false, o)))
        .filter(|(_, o)| map.keys().any(|k| k == &o.unit.pkg.package_id()))
    {
        create_component(config, &output.path, binary)?;
    }

    Ok(())
}

/// Generate API documentation for the given workspace and compile options.
///
/// It is expected that the current package contains a `package.metadata.component` section.
pub async fn doc(
    config: &Config,
    mut workspace: Workspace<'_>,
    options: &DocOptions,
    force_generation: bool,
) -> Result<()> {
    generate_workspace_bindings(config, &mut workspace, force_generation).await?;
    ops::doc(&workspace, options)?;
    Ok(())
}

/// Retrieves workspace metadata for the given workspace and metadata options.
///
/// The returned metadata contains information about generated dependencies.
pub async fn metadata(
    config: &Config,
    mut workspace: Workspace<'_>,
    options: &OutputMetadataOptions,
) -> Result<ExportInfo> {
    generate_workspace_bindings(config, &mut workspace, false).await?;
    ops::output_metadata(&workspace, options)
}

/// Check a component for errors with the given workspace and compile options.
pub async fn check(
    config: &Config,
    mut workspace: Workspace<'_>,
    options: &CompileOptions,
    force_generation: bool,
) -> Result<()> {
    generate_workspace_bindings(config, &mut workspace, force_generation).await?;
    ops::compile(&workspace, options)?;
    Ok(())
}

/// Update the dependencies in the local lock files.
///
/// This updates both `Cargo.lock` and `Cargo-component.lock`.
pub async fn update_lockfile(
    config: &Config,
    workspace: &Workspace<'_>,
    options: &UpdateOptions<'_>,
) -> Result<()> {
    // First update `Cargo.lock`
    ops::update_lockfile(workspace, options)?;

    // Next read the current lock file and generate a new one
    let map = create_resolution_map(config, workspace, None).await?;
    let orig_lock_file = LockFile::open(config, workspace)?.unwrap_or_default();
    let new_lock_file = LockFile::from_resolution(&map);

    // Unlike `cargo`, the lock file doesn't have transitive dependencies
    // So we expect the entries (and the version requirements) to be the same
    // Thus, only "updating" messages get printed for the packages that changed
    for old_pkg in &orig_lock_file.packages {
        let new_pkg_index = new_lock_file
            .packages
            .binary_search_by_key(&old_pkg.key(), LockedPackage::key)
            .expect("locked packages should remain the same");

        let new_pkg = &new_lock_file.packages[new_pkg_index];
        for old_ver in &old_pkg.versions {
            let new_ver_index = new_pkg
                .versions
                .binary_search_by_key(&old_ver.key(), LockedPackageVersion::key)
                .expect("version requirements should remain the same");

            let new_ver = &new_pkg.versions[new_ver_index];
            if old_ver.version != new_ver.version {
                config.shell().status_with_color(
                    "Updating",
                    format!(
                        "component registry package `{id}` v{old} -> v{new}",
                        id = old_pkg.id,
                        old = old_ver.version,
                        new = new_ver.version
                    ),
                    Color::Green,
                )?;
            }
        }
    }

    if options.dry_run {
        options
            .config
            .shell()
            .warn("not updating component lock file due to dry run")?;
    } else {
        new_lock_file.update(config, workspace, &orig_lock_file)?;
    }

    Ok(())
}
