//! Cargo support for WebAssembly components.

#![deny(missing_docs)]

use crate::target::install_wasm32_wasi;
use anyhow::{bail, Context, Result};
use bindings::BindingsGenerator;
use bytes::Bytes;
use cargo_component_core::{
    lock::{LockFile, LockFileResolver, LockedPackage, LockedPackageVersion},
    registry::create_client,
    terminal::Colors,
};
use cargo_config2::{PathAndArgs, TargetTripleRef};
use cargo_metadata::{Message, Metadata, MetadataCommand, Package};
use config::{CargoArguments, CargoPackageSpec, Config};
use lock::{acquire_lock_file_ro, acquire_lock_file_rw};
use metadata::ComponentMetadata;
use registry::{PackageDependencyResolution, PackageResolutionMap};
use semver::Version;
use std::{
    borrow::Cow,
    env, fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime},
};
use warg_client::storage::{ContentStorage, PublishEntry, PublishInfo};
use warg_crypto::signing::PrivateKey;
use warg_protocol::registry::PackageId;
use wasm_metadata::{Link, LinkType, RegistryMetadata};
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

/// Represents a cargo build artifact.
struct BuildArtifact {
    /// The path to the artifact.
    path: PathBuf,
    /// The package that this artifact was compiled for.
    package: String,
    /// The target that this artifact was compiled for.
    target: String,
    /// Whether or not this artifact was `fresh` during this build.
    fresh: bool,
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
    generate_bindings(config, metadata, packages, cargo_args).await?;

    let cargo = std::env::var("CARGO")
        .map(PathBuf::from)
        .ok()
        .unwrap_or_else(|| PathBuf::from("cargo"));

    let is_build = matches!(subcommand, Some("b") | Some("build") | Some("rustc"));
    let is_run = matches!(subcommand, Some("r") | Some("run"));
    let is_test = matches!(subcommand, Some("t") | Some("test") | Some("bench"));

    let (build_args, runtime_args) = match spawn_args.iter().position(|a| a == "--") {
        Some(position) => spawn_args.split_at(position),
        None => (spawn_args, &[] as &[String]),
    };
    let needs_runner = !build_args.iter().any(|a| a == "--no-run");

    let mut args = build_args.iter().peekable();
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
    if is_run {
        cmd.arg("build");
        if let Some(arg) = args.peek() {
            if Some((*arg).as_str()) == subcommand {
                args.next().unwrap();
            }
        }
    }
    cmd.args(args);

    // TODO: consider targets from .cargo/config.toml

    // Handle the target for build, run and test commands
    if is_build || is_run || is_test {
        install_wasm32_wasi(config)?;

        // Add an implicit wasm32-wasi target if there isn't a wasm target present
        if !cargo_args.targets.iter().any(|t| is_wasm_target(t)) {
            cmd.arg("--target").arg("wasm32-wasi");
        }

        if let Some(format) = &cargo_args.message_format {
            if format != "json-render-diagnostics" {
                bail!("unsupported cargo message format `{format}`");
            }
        }

        // It will output the message as json so we can extract the wasm files
        // that will be componentized
        cmd.arg("--message-format").arg("json-render-diagnostics");
        cmd.stdout(Stdio::piped());
    } else {
        cmd.stdout(Stdio::inherit());
    }

    if needs_runner && is_test {
        // Only build for the test target; running will be handled
        // after the componentization
        cmd.arg("--no-run");
    }

    let mut runner: Option<PathAndArgs> = None;
    if needs_runner && (is_run || is_test) {
        let cargo_config = cargo_config2::Config::load()?;

        // We check here before we actually build that a runtime is present.
        // We first check the runner for `wasm32-wasi` in the order from
        // cargo's convention for a user-supplied runtime (path or executable)
        // and use the default, namely `wasmtime`, if it is not set.
        let (r, using_default) = cargo_config
            .runner(TargetTripleRef::from("wasm32-wasi"))
            .unwrap_or_default()
            .map(|runner_override| (runner_override, false))
            .unwrap_or_else(|| {
                (
                    PathAndArgs::new("wasmtime")
                        .args(vec![
                            "-W",
                            "component-model",
                            "-S",
                            "preview2",
                            "-S",
                            "common",
                        ])
                        .to_owned(),
                    true,
                )
            });
        runner = Some(r.clone());

        // Treat the runner object as an executable with list of arguments it
        // that was extracted by splitting each whitespace. This allows the user
        // to provide arguments which are passed to wasmtime without having to
        // add more command-line argument parsing to this crate.
        let wasi_runner = r.path.to_string_lossy().into_owned();

        if !using_default {
            // check if the override runner exists
            if !(r.path.exists() || which::which(r.path).is_ok()) {
                bail!(
                    "failed to find `{wasi_runner}` specified by either the `CARGO_TARGET_WASM32_WASI_RUNNER`\
                    environment variable or as the `wasm32-wasi` runner in `.cargo/config.toml`"
                );
            }
        } else if which::which(r.path).is_err() {
            bail!(
                "failed to find `{wasi_runner}` on PATH\n\n\
                 ensure Wasmtime is installed before running this command\n\n\
                 {msg}:\n\n  {instructions}",
                msg = if cfg!(unix) {
                    "Wasmtime can be installed via a shell script"
                } else {
                    "Wasmtime can be installed via the GitHub releases page"
                },
                instructions = if cfg!(unix) {
                    "curl https://wasmtime.dev/install.sh -sSf | bash"
                } else {
                    "https://github.com/bytecodealliance/wasmtime/releases"
                },
            );
        }
    }

    let mut outputs = Vec::new();
    log::debug!("spawning command {:?}", cmd);

    let mut child = cmd.spawn().context(format!(
        "failed to spawn `{cargo}`",
        cargo = cargo.display()
    ))?;

    let mut artifacts = Vec::new();

    if is_build || is_run || is_test {
        let stdout = child.stdout.take().expect("no stdout");
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = line.context("failed to read output from `cargo`")?;

            // If the command line arguments also had `--message-format`, echo the line
            if cargo_args.message_format.is_some() {
                println!("{line}");
            }

            if line.is_empty() {
                continue;
            }

            for message in Message::parse_stream(line.as_bytes()) {
                if let Message::CompilerArtifact(artifact) =
                    message.context("unexpected JSON message from cargo")?
                {
                    for path in artifact.filenames {
                        let path = PathBuf::from(path);
                        if path.extension().and_then(|s| s.to_str()) == Some("wasm") {
                            log::debug!(
                                "found WebAssembly build artifact `{path}`",
                                path = path.display()
                            );
                            artifacts.push(BuildArtifact {
                                path,
                                package: artifact.package_id.to_string(),
                                target: artifact.target.name.clone(),
                                fresh: artifact.fresh,
                            });
                        }
                    }
                }
            }
        }
    }

    let status = child.wait().context(format!(
        "failed to wait for `{cargo}` to finish",
        cargo = cargo.display()
    ))?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    for artifact in &artifacts {
        if artifact.path.exists() {
            for PackageComponentMetadata { package, metadata } in packages {
                // When passing `--bin` the target name will be the binary being executed,
                // but the package id still points to the package the binary is part of.
                if artifact.target == package.name || artifact.package.starts_with(&package.name) {
                    if let Some(metadata) = &metadata {
                        let is_bin = is_test || package.targets.iter().any(|t| t.is_bin());
                        let bytes = &mut fs::read(&artifact.path).with_context(|| {
                            format!(
                                "failed to read output module `{path}`",
                                path = artifact.path.display()
                            )
                        })?;

                        // If the compilation output is not a WebAssembly module, then do nothing
                        // Note: due to the way cargo currently works on macOS, it will overwrite
                        // a previously generated component on an up-to-date build.
                        //
                        // Thus we always componentize the artifact on macOS, but we only print
                        // the status message if the artifact was not fresh.
                        //
                        // See: https://github.com/rust-lang/cargo/blob/99ad42deb4b0be0cdb062d333d5e63460a94c33c/crates/cargo-util/src/paths.rs#L542-L550
                        if bytes.len() < 8 || bytes[0..4] != [0x0, b'a', b's', b'm'] {
                            bail!(
                                "expected `{path}` to be a WebAssembly module or component",
                                path = artifact.path.display()
                            );
                        }

                        // Check for the module header version
                        if bytes[4..8] == [0x01, 0x00, 0x00, 0x00] {
                            create_component(
                                config,
                                metadata,
                                &artifact.path,
                                is_bin,
                                artifact.fresh,
                            )?;
                        } else {
                            log::debug!(
                                "output file `{path}` is already a WebAssembly component",
                                path = artifact.path.display()
                            );
                        }
                    }

                    outputs.push(artifact.path.clone());
                }
            }
        }
    }

    for PackageComponentMetadata {
        package,
        metadata: _,
    } in packages
    {
        if !artifacts.iter().any(
            |BuildArtifact {
                 package: output, ..
             }| output.starts_with(&package.name),
        ) {
            log::warn!(
                "no build output found for package `{name}`",
                name = package.name
            );
        }
    }

    if let Some(runner) = runner {
        for run in outputs.iter() {
            let mut cmd = Command::new(&runner.path);
            cmd.args(&runner.args)
                .arg("--")
                .arg(run)
                .args(runtime_args.iter().skip(1))
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            log::debug!("spawning command {:?}", cmd);

            let mut child = cmd.spawn().context(format!(
                "failed to spawn `{runner}`",
                runner = runner.path.display()
            ))?;

            let status = child.wait().context(format!(
                "failed to wait for `{runner}` to finish",
                runner = runner.path.display()
            ))?;

            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
        }
    }

    Ok(outputs)
}

fn last_modified_time(path: &Path) -> Result<SystemTime> {
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

async fn generate_bindings(
    config: &Config,
    metadata: &Metadata,
    packages: &[PackageComponentMetadata<'_>],
    cargo_args: &CargoArguments,
) -> Result<()> {
    let last_modified_exe = last_modified_time(&std::env::current_exe()?)?;
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

        generate_package_bindings(config, resolution, last_modified_exe).await?;
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

async fn generate_package_bindings(
    config: &Config,
    resolution: &PackageDependencyResolution<'_>,
    last_modified_exe: SystemTime,
) -> Result<()> {
    // TODO: make the output path configurable
    let output_dir = resolution
        .metadata
        .manifest_path
        .parent()
        .unwrap()
        .join("src");
    let bindings_path = output_dir.join("bindings.rs");

    let last_modified_output = bindings_path
        .is_file()
        .then(|| last_modified_time(&bindings_path))
        .transpose()?
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let generator = BindingsGenerator::new(resolution)?;
    match generator.reason(last_modified_exe, last_modified_output)? {
        Some(reason) => {
            ::log::debug!(
                "generating bindings for package `{name}` at `{path}` because {reason}",
                name = resolution.metadata.name,
                path = bindings_path.display(),
            );

            config.terminal().status(
                "Generating",
                format!(
                    "bindings for {name} ({path})",
                    name = resolution.metadata.name,
                    path = bindings_path.display()
                ),
            )?;

            let bindings = generator.generate()?;
            fs::create_dir_all(&output_dir).with_context(|| {
                format!(
                    "failed to create output directory `{path}`",
                    path = output_dir.display()
                )
            })?;

            fs::write(&bindings_path, bindings).with_context(|| {
                format!(
                    "failed to write bindings file `{path}`",
                    path = bindings_path.display()
                )
            })?;
        }
        None => {
            ::log::debug!(
                "existing bindings for package `{name}` at `{path}` is up-to-date",
                name = resolution.metadata.name,
                path = bindings_path.display(),
            );
        }
    }

    Ok(())
}

fn adapter_bytes<'a>(
    config: &Config,
    metadata: &'a ComponentMetadata,
    binary: bool,
) -> Result<Cow<'a, [u8]>> {
    if let Some(adapter) = &metadata.section.adapter {
        if metadata.section.proxy {
            config.terminal().warn(
                "ignoring `proxy` setting due to `adapter` setting being present in `Cargo.toml`",
            )?;
        }

        return Ok(fs::read(adapter)
            .with_context(|| {
                format!(
                    "failed to read module adapter `{path}`",
                    path = adapter.display()
                )
            })?
            .into());
    }

    if binary {
        if metadata.section.proxy {
            config
                .terminal()
                .warn("ignoring `proxy` setting in `Cargo.toml` for command component")?;
        }

        Ok(Cow::Borrowed(include_bytes!(concat!(
            "../adapters/",
            env!("WASI_ADAPTER_VERSION"),
            "/wasi_snapshot_preview1.command.wasm"
        ))))
    } else if metadata.section.proxy {
        Ok(Cow::Borrowed(include_bytes!(concat!(
            "../adapters/",
            env!("WASI_ADAPTER_VERSION"),
            "/wasi_snapshot_preview1.proxy.wasm"
        ))))
    } else {
        Ok(Cow::Borrowed(include_bytes!(concat!(
            "../adapters/",
            env!("WASI_ADAPTER_VERSION"),
            "/wasi_snapshot_preview1.reactor.wasm"
        ))))
    }
}

fn create_component(
    config: &Config,
    metadata: &ComponentMetadata,
    path: &Path,
    binary: bool,
    fresh: bool,
) -> Result<()> {
    ::log::debug!(
        "componentizing WebAssembly module `{path}` as a {kind} component (fresh = {fresh})",
        path = path.display(),
        kind = if binary { "command" } else { "reactor" },
    );

    let module = fs::read(path).with_context(|| {
        format!(
            "failed to read output module `{path}`",
            path = path.display()
        )
    })?;

    // Only print the message if the artifact was not fresh
    if !fresh {
        config.terminal().status(
            "Creating",
            format!("component {path}", path = path.display()),
        )?;
    }

    let encoder = ComponentEncoder::default()
        .module(&module)?
        .adapter(
            "wasi_snapshot_preview1",
            &adapter_bytes(config, metadata, binary)?,
        )
        .with_context(|| {
            format!(
                "failed to load adapter module `{path}`",
                path = if let Some(path) = &metadata.section.adapter {
                    path.as_path()
                } else {
                    Path::new("<built-in>")
                }
                .display()
            )
        })?
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
    /// The package to publish.
    pub package: &'a Package,
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

fn add_registry_metadata(package: &Package, bytes: &[u8], path: &Path) -> Result<Vec<u8>> {
    let mut metadata = RegistryMetadata::default();
    if !package.authors.is_empty() {
        metadata.set_authors(Some(package.authors.clone()));
    }

    if !package.categories.is_empty() {
        metadata.set_categories(Some(package.categories.clone()));
    }

    metadata.set_description(package.description.clone());

    // TODO: registry metadata should have keywords
    // if !package.keywords.is_empty() {
    //     metadata.set_keywords(Some(package.keywords.clone()));
    // }

    metadata.set_license(package.license.clone());

    let mut links = Vec::new();
    if let Some(docs) = &package.documentation {
        links.push(Link {
            ty: LinkType::Documentation,
            value: docs.clone(),
        });
    }

    if let Some(homepage) = &package.homepage {
        links.push(Link {
            ty: LinkType::Homepage,
            value: homepage.clone(),
        });
    }

    if let Some(repo) = &package.repository {
        links.push(Link {
            ty: LinkType::Repository,
            value: repo.clone(),
        });
    }

    if !links.is_empty() {
        metadata.set_links(Some(links));
    }

    metadata.add_to_wasm(bytes).with_context(|| {
        format!(
            "failed to add registry metadata to component `{path}`",
            path = path.display()
        )
    })
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

    let bytes = fs::read(options.path).with_context(|| {
        format!(
            "failed to read component `{path}`",
            path = options.path.display()
        )
    })?;

    let bytes = add_registry_metadata(options.package, &bytes, options.path)?;

    let content = client
        .content()
        .store_content(
            Box::pin(futures::stream::once(async { Ok(Bytes::from(bytes)) })),
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
