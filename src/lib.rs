//! Cargo support for WebAssembly components.

#![deny(missing_docs)]

use crate::target::install_wasm32_wasi;
use anyhow::{anyhow, bail, Context, Result};
use bindings::BindingsEncoder;
use cargo_component_core::{
    lock::{LockFile, LockFileResolver, LockedPackage, LockedPackageVersion},
    registry::create_client,
    terminal::Colors,
};
use cargo_metadata::{Metadata, MetadataCommand, Package};
use config::{CargoArguments, CargoPackageSpec, Config};
use futures::TryStreamExt;
use lock::{acquire_lock_file_ro, acquire_lock_file_rw};
use metadata::ComponentMetadata;
use registry::{PackageDependencyResolution, PackageResolutionMap};
use semver::Version;
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};
use tokio::io::BufReader;
use tokio_util::io::ReaderStream;
use warg_client::storage::{ContentStorage, PublishEntry, PublishInfo};
use warg_crypto::signing::PrivateKey;
use warg_protocol::registry::PackageId;
use wit_component::ComponentEncoder;

mod bindings;
pub mod commands;
pub mod config;
mod generator;
mod lock;
mod metadata;
mod registry;
mod target;

fn is_wasm_target(target: &str) -> bool {
    target == "wasm32-wasi" || target == "wasm32-unknown-unknown"
}

/// Represents a cargo package paired with its component metadata.
pub struct PackageComponentMetadata<'a> {
    /// The associated package.
    pub package: &'a Package,
    /// The associated component metadata.
    ///
    /// This is `None` if the package is not a component.
    pub metadata: Option<ComponentMetadata>,
}

impl<'a> PackageComponentMetadata<'a> {
    /// Creates a new package metadata from the given package.
    pub fn new(package: &'a Package) -> Result<Self> {
        Ok(Self {
            package,
            metadata: ComponentMetadata::from_package(package)?,
        })
    }
}

/// Runs the cargo command as specified in the configuration.
///
/// Note: if the command returns a non-zero status, this
/// function will exit the process.
///
/// Returns any relevant output components.
pub async fn run_cargo_command(
    config: &Config,
    metadata: &Metadata,
    packages: &[PackageComponentMetadata<'_>],
    subcommand: Option<&str>,
    cargo_args: &CargoArguments,
    spawn_args: &[String],
) -> Result<Vec<PathBuf>> {
    encode_targets(config, metadata, packages, cargo_args).await?;

    let cargo = std::env::var("CARGO")
        .map(PathBuf::from)
        .ok()
        .unwrap_or_else(|| PathBuf::from("cargo"));

    let mut args = spawn_args.iter().peekable();
    if let Some(arg) = args.peek() {
        if *arg == "component" {
            args.next().unwrap();
        }
    }

    // Spawn the actual cargo command
    log::debug!(
        "spawning cargo `{cargo}` with arguments `{args:?}`",
        cargo = cargo.display(),
        args = args.clone().collect::<Vec<_>>(),
    );

    let mut cmd = Command::new(&cargo);
    cmd.args(args);

    let is_build = matches!(subcommand, Some("b") | Some("build") | Some("rustc"));

    // Handle the target for build commands
    if is_build {
        install_wasm32_wasi(config)?;

        // Add an implicit wasm32-wasi target if there isn't a wasm target present
        if !cargo_args.targets.iter().any(|t| is_wasm_target(t)) {
            cmd.arg("--target").arg("wasm32-wasi");
        }
    }

    match cmd.status() {
        Ok(status) => {
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
        }
        Err(e) => {
            bail!("failed to spawn `{cargo}`: {e}", cargo = cargo.display());
        }
    }

    let mut outputs = Vec::new();
    if is_build {
        log::debug!("searching for WebAssembly modules to componentize");
        let targets = cargo_args
            .targets
            .iter()
            .map(String::as_str)
            .filter(|t| is_wasm_target(t))
            .chain(cargo_args.targets.is_empty().then_some("wasm32-wasi"));

        for target in targets {
            let out_dir = metadata
                .target_directory
                .join(target)
                .join(if cargo_args.release {
                    "release"
                } else {
                    "debug"
                });

            for PackageComponentMetadata { package, .. } in packages {
                let is_bin = package.targets.iter().any(|t| t.is_bin());

                // First try for <name>.wasm
                let path = out_dir.join(&package.name).with_extension("wasm");
                if path.exists() {
                    create_component(config, path.as_std_path(), is_bin)?;
                    outputs.push(path.to_path_buf().into_std_path_buf());
                    continue;
                }

                // Next, try replacing `-` with `_`
                let path = out_dir
                    .join(package.name.replace('-', "_"))
                    .with_extension("wasm");
                if path.exists() {
                    create_component(config, path.as_std_path(), is_bin)?;
                    outputs.push(path.to_path_buf().into_std_path_buf());
                    continue;
                }

                log::debug!("no output found for package `{name}`", name = package.name);
            }
        }
    }

    Ok(outputs)
}

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

/// Loads the workspace metadata based on the given manifest path.
pub fn load_metadata(manifest_path: Option<&Path>) -> Result<Metadata> {
    let mut command = MetadataCommand::new();
    command.no_deps();

    if let Some(path) = manifest_path {
        log::debug!(
            "loading metadata from manifest `{path}`",
            path = path.display()
        );
        command.manifest_path(path);
    } else {
        log::debug!("loading metadata from current directory");
    }

    command.exec().context("failed to load cargo metadata")
}

/// Loads the component metadata for the given package specs.
///
/// If `workspace` is true, all workspace packages are loaded.
pub fn load_component_metadata<'a>(
    metadata: &'a Metadata,
    specs: impl ExactSizeIterator<Item = &'a CargoPackageSpec>,
    workspace: bool,
) -> Result<Vec<PackageComponentMetadata<'a>>> {
    let pkgs = if workspace {
        metadata.workspace_packages()
    } else if specs.len() > 0 {
        let mut pkgs = Vec::with_capacity(specs.len());
        for spec in specs {
            let pkg = metadata
                .packages
                .iter()
                .find(|p| {
                    p.name == spec.name
                        && match spec.version.as_ref() {
                            Some(v) => &p.version == v,
                            None => true,
                        }
                })
                .with_context(|| {
                    format!("package ID specification `{spec}` did not match any packages")
                })?;
            pkgs.push(pkg);
        }

        pkgs
    } else {
        // TODO: this should be the default members, or default to all members
        // However, `cargo-metadata` doesn't return the workspace default members yet
        // See: https://github.com/oli-obk/cargo_metadata/issues/215
        metadata.workspace_packages()
    };

    pkgs.into_iter()
        .map(PackageComponentMetadata::new)
        .collect::<Result<_>>()
}

async fn encode_targets(
    config: &Config,
    metadata: &Metadata,
    packages: &[PackageComponentMetadata<'_>],
    cargo_args: &CargoArguments,
) -> Result<()> {
    let bindings_dir = metadata.target_directory.join("bindings");
    let file_lock = acquire_lock_file_ro(config.terminal(), metadata)?;
    let lock_file = file_lock
        .as_ref()
        .map(|f| {
            LockFile::read(f.file()).with_context(|| {
                format!(
                    "failed to read lock file `{path}`",
                    path = f.path().display()
                )
            })
        })
        .transpose()?;

    let resolver = lock_file.as_ref().map(LockFileResolver::new);
    let map =
        create_resolution_map(config, packages, resolver, cargo_args.network_allowed()).await?;
    for PackageComponentMetadata { package, .. } in packages {
        let resolution = match map.get(&package.id) {
            Some(resolution) => resolution,
            None => continue,
        };

        encode_target_world(config, resolution, bindings_dir.as_std_path()).await?;
    }

    // Update the lock file if it exists or if the new lock file is non-empty
    let new_lock_file = map.to_lock_file();
    if (lock_file.is_some() || !new_lock_file.packages.is_empty())
        && Some(&new_lock_file) != lock_file.as_ref()
    {
        drop(file_lock);
        let file_lock = acquire_lock_file_rw(
            config.terminal(),
            metadata,
            cargo_args.lock_update_allowed(),
            cargo_args.locked,
        )?;
        new_lock_file
            .write(file_lock.file(), "cargo-component")
            .with_context(|| {
                format!(
                    "failed to write lock file `{path}`",
                    path = file_lock.path().display()
                )
            })?;
    }

    Ok(())
}

async fn create_resolution_map<'a>(
    config: &Config,
    packages: &'a [PackageComponentMetadata<'_>],
    lock_file: Option<LockFileResolver<'_>>,
    network_allowed: bool,
) -> Result<PackageResolutionMap<'a>> {
    let mut map = PackageResolutionMap::default();

    for PackageComponentMetadata { package, metadata } in packages {
        match metadata {
            Some(metadata) => {
                let resolution =
                    PackageDependencyResolution::new(config, metadata, lock_file, network_allowed)
                        .await?;
                map.insert(package.id.clone(), resolution);
            }
            None => continue,
        }
    }

    Ok(map)
}

async fn encode_target_world(
    config: &Config,
    resolution: &PackageDependencyResolution<'_>,
    bindings_dir: &Path,
) -> Result<()> {
    let output_dir = bindings_dir.join(&resolution.metadata.name);
    let target_path = output_dir.join("target.wasm");
    let world_path = output_dir.join("world");

    let last_modified_output = target_path
        .is_file()
        .then(|| last_modified_time(&target_path))
        .transpose()?
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let encoder = BindingsEncoder::new(resolution)?;
    match encoder.reason(last_modified_output)? {
        Some(reason) => {
            ::log::debug!(
                "encoding target for package `{name}` at `{path}` because {reason}",
                name = resolution.metadata.name,
                path = target_path.display(),
            );

            config.terminal().status(
                "Encoding",
                format!(
                    "target for {name} ({path})",
                    name = resolution.metadata.name,
                    path = target_path.display()
                ),
            )?;

            let encoded = encoder.encode(None)?;
            fs::create_dir_all(&output_dir).with_context(|| {
                format!(
                    "failed to create output directory `{path}`",
                    path = output_dir.display()
                )
            })?;

            fs::write(&target_path, encoded).with_context(|| {
                format!(
                    "failed to write target file `{path}`",
                    path = target_path.display()
                )
            })?;

            let world = resolution
                .metadata
                .section
                .target
                .as_ref()
                .and_then(|t| t.world())
                .unwrap_or("");

            fs::write(&world_path, world).with_context(|| {
                format!(
                    "failed to write world name `{path}`",
                    path = world_path.display()
                )
            })?;
        }
        None => {
            ::log::debug!(
                "existing target encoding for package `{name}` at `{path}` is up-to-date",
                name = resolution.metadata.name,
                path = target_path.display(),
            );
        }
    }

    Ok(())
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
        "componentizing WebAssembly module `{path}` as a {kind} component",
        path = path.display(),
        kind = if binary { "command" } else { "reactor" },
    );

    let module = fs::read(path).with_context(|| {
        format!(
            "failed to read output module `{path}`",
            path = path.display()
        )
    })?;

    config.terminal().status(
        "Creating",
        format!("component {path}", path = path.display()),
    )?;

    let encoder = ComponentEncoder::default()
        .adapter(
            "wasi_snapshot_preview1",
            if binary {
                include_bytes!(concat!(
                    "../adapters/",
                    env!("WASI_ADAPTER_VERSION"),
                    "/wasi_snapshot_preview1.command.wasm"
                ))
            } else {
                include_bytes!(concat!(
                    "../adapters/",
                    env!("WASI_ADAPTER_VERSION"),
                    "/wasi_snapshot_preview1.reactor.wasm"
                ))
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
        format!(
            "failed to add metadata to output component `{path}`",
            path = path.display()
        )
    })?;

    fs::write(path, component).with_context(|| {
        format!(
            "failed to write output component `{path}`",
            path = path.display()
        )
    })
}

/// Represents options for a publish operation.
pub struct PublishOptions<'a> {
    /// The registry URL to publish to.
    pub registry_url: &'a str,
    /// Whether to initialize the package or not.
    pub init: bool,
    /// The id of the package being published.
    pub id: &'a PackageId,
    /// The version of the package being published.
    pub version: &'a Version,
    /// The path to the package being published.
    pub path: &'a Path,
    /// The signing key to use for the publish operation.
    pub signing_key: &'a PrivateKey,
    /// Whether to perform a dry run or not.
    pub dry_run: bool,
}

/// Publish a component for the given workspace and publish options.
pub async fn publish(config: &Config, options: &PublishOptions<'_>) -> Result<()> {
    if options.dry_run {
        config
            .terminal()
            .warn("not publishing component to the registry due to the --dry-run option")?;
        return Ok(());
    }

    let client = create_client(config.warg(), options.registry_url, config.terminal())?;

    let content = client
        .content()
        .store_content(
            Box::pin(
                ReaderStream::new(BufReader::new(
                    tokio::fs::File::open(options.path).await.with_context(|| {
                        format!("failed to open `{path}`", path = options.path.display())
                    })?,
                ))
                .map_err(|e| anyhow!(e)),
            ),
            None,
        )
        .await?;

    config.terminal().status(
        "Publishing",
        format!(
            "component {path} ({content})",
            path = options.path.display()
        ),
    )?;

    let mut info = PublishInfo {
        id: options.id.clone(),
        head: None,
        entries: Default::default(),
    };

    if options.init {
        info.entries.push(PublishEntry::Init);
    }

    info.entries.push(PublishEntry::Release {
        version: options.version.clone(),
        content,
    });

    let record_id = client.publish_with_info(options.signing_key, info).await?;
    client
        .wait_for_publish(options.id, &record_id, Duration::from_secs(1))
        .await?;

    config.terminal().status(
        "Published",
        format!(
            "package `{id}` v{version}",
            id = options.id,
            version = options.version
        ),
    )?;

    Ok(())
}

/// Update the dependencies in the lock file.
///
/// This updates only `Cargo-component.lock`.
pub async fn update_lockfile(
    config: &Config,
    metadata: &Metadata,
    packages: &[PackageComponentMetadata<'_>],
    network_allowed: bool,
    lock_update_allowed: bool,
    locked: bool,
    dry_run: bool,
) -> Result<()> {
    // Read the current lock file and generate a new one
    let map = create_resolution_map(config, packages, None, network_allowed).await?;

    let file_lock = acquire_lock_file_ro(config.terminal(), metadata)?;
    let orig_lock_file = file_lock
        .as_ref()
        .map(|f| {
            LockFile::read(f.file()).with_context(|| {
                format!(
                    "failed to read lock file `{path}`",
                    path = f.path().display()
                )
            })
        })
        .transpose()?
        .unwrap_or_default();

    let new_lock_file = map.to_lock_file();

    for old_pkg in &orig_lock_file.packages {
        let new_pkg = match new_lock_file
            .packages
            .binary_search_by_key(&old_pkg.key(), LockedPackage::key)
            .map(|index| &new_lock_file.packages[index])
        {
            Ok(pkg) => pkg,
            Err(_) => {
                // The package is no longer a dependency
                for old_ver in &old_pkg.versions {
                    config.terminal().status_with_color(
                        if dry_run { "Would remove" } else { "Removing" },
                        format!(
                            "dependency `{id}` v{version}",
                            id = old_pkg.id,
                            version = old_ver.version,
                        ),
                        Colors::Red,
                    )?;
                }
                continue;
            }
        };

        for old_ver in &old_pkg.versions {
            let new_ver = match new_pkg
                .versions
                .binary_search_by_key(&old_ver.key(), LockedPackageVersion::key)
                .map(|index| &new_pkg.versions[index])
            {
                Ok(ver) => ver,
                Err(_) => {
                    // The version of the package is no longer a dependency
                    config.terminal().status_with_color(
                        if dry_run { "Would remove" } else { "Removing" },
                        format!(
                            "dependency `{id}` v{version}",
                            id = old_pkg.id,
                            version = old_ver.version,
                        ),
                        Colors::Red,
                    )?;
                    continue;
                }
            };

            // The version has changed
            if old_ver.version != new_ver.version {
                config.terminal().status_with_color(
                    if dry_run { "Would update" } else { "Updating" },
                    format!(
                        "dependency `{id}` v{old} -> v{new}",
                        id = old_pkg.id,
                        old = old_ver.version,
                        new = new_ver.version
                    ),
                    Colors::Cyan,
                )?;
            }
        }
    }

    for new_pkg in &new_lock_file.packages {
        let old_pkg = match orig_lock_file
            .packages
            .binary_search_by_key(&new_pkg.key(), LockedPackage::key)
            .map(|index| &orig_lock_file.packages[index])
        {
            Ok(pkg) => pkg,
            Err(_) => {
                // The package is new
                for new_ver in &new_pkg.versions {
                    config.terminal().status_with_color(
                        if dry_run { "Would add" } else { "Adding" },
                        format!(
                            "dependency `{id}` v{version}",
                            id = new_pkg.id,
                            version = new_ver.version,
                        ),
                        Colors::Green,
                    )?;
                }
                continue;
            }
        };

        for new_ver in &new_pkg.versions {
            if old_pkg
                .versions
                .binary_search_by_key(&new_ver.key(), LockedPackageVersion::key)
                .map(|index| &old_pkg.versions[index])
                .is_err()
            {
                // The version is new
                config.terminal().status_with_color(
                    if dry_run { "Would add" } else { "Adding" },
                    format!(
                        "dependency `{id}` v{version}",
                        id = new_pkg.id,
                        version = new_ver.version,
                    ),
                    Colors::Green,
                )?;
            }
        }
    }

    if dry_run {
        config
            .terminal()
            .warn("not updating component lock file due to --dry-run option")?;
    } else {
        // Update the lock file
        if new_lock_file != orig_lock_file {
            drop(file_lock);
            let file_lock =
                acquire_lock_file_rw(config.terminal(), metadata, lock_update_allowed, locked)?;
            new_lock_file
                .write(file_lock.file(), "cargo-component")
                .with_context(|| {
                    format!(
                        "failed to write lock file `{path}`",
                        path = file_lock.path().display()
                    )
                })?;
        }
    }

    Ok(())
}
