//! Cargo support for WebAssembly components.

#![deny(missing_docs)]

use crate::target::install_wasm32_wasi;
use anyhow::{bail, Context, Result};
use bindings::BindingsGenerator;
use bytes::Bytes;
use cargo_component_core::{
    lock::{LockFile, LockFileResolver, LockedPackage, LockedPackageVersion},
    registry::create_client,
    terminal::{Colors, Terminal, Verbosity},
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
    env,
    ffi::OsStr,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
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

/// The name of the cargo-component bindings crate.
pub const BINDINGS_CRATE_NAME: &str = "cargo-component-bindings";

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
    generate_bindings(config, metadata, packages, cargo_args).await?;

    let cargo = std::env::var("CARGO")
        .map(PathBuf::from)
        .ok()
        .unwrap_or_else(|| PathBuf::from("cargo"));

    let is_build = matches!(subcommand, Some("b") | Some("build") | Some("rustc"));
    let is_run = matches!(subcommand, Some("run"));
    let is_test = matches!(subcommand, Some("test") | Some("bench"));

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

        // It will output the message as json so we can extract the wasm files
        // that will be componentized
        cmd.arg("--message-format").arg("json-render-diagnostics");
    }

    if needs_runner && is_test {
        cmd.arg("--no-run");
        cmd.env("CARGO_TARGET_WASM32_WASI_RUNNER", "echo");
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
            // check if the override is either a valid path or command found on $PATH
            if !(r.path.exists() || which::which(r.path).is_ok()) {
                bail!(
                    "failed to find `{}` (specified by $CARGO_TARGET_WASM32_WASI_RUNNER) \
                        on the filesytem or in $PATH, you'll want to fix the path or unset \
                        the $CARGO_TARGET_WASM32_WASI_RUNNER environment variable before \
                        running this command\n",
                    wasi_runner
                );
            }
        } else if which::which(r.path).is_err() {
            let mut msg = format!(
                "failed to find `{}` in $PATH, you'll want to \
                    install `{}` before running this command\n",
                wasi_runner, wasi_runner
            );
            // Because we know what runtime is being used here, we can print
            // out installation information.
            if cfg!(unix) {
                msg.push_str("you can also install through a shell:\n\n");
                msg.push_str("\tcurl https://wasmtime.dev/install.sh -sSf | bash\n");
            } else {
                msg.push_str("you can also install through the installer:\n\n");
                msg.push_str("\thttps://github.com/bytecodealliance/wasmtime/releases/download/dev/wasmtime-dev-x86_64-windows.msi\n");
            }
            bail!("{}", msg);
        }
    }

    let mut outputs = Vec::new();
    let mut cmd_output = "".to_string();
    log::debug!("spawning command {:?}", cmd);

    let output = cmd.output().context(format!(
        "failed to spawn `{cargo}`",
        cargo = cargo.display()
    ))?;

    if let Ok(value) = String::from_utf8(output.stdout) {
        cmd_output = value;
    }
    if !(is_build || is_run || is_test) || config.terminal().verbosity() == Verbosity::Verbose {
        log::trace!("--- cargo command stdout [BEGIN] ---");
        println!("{}", cmd_output);
        log::trace!("--- cargo command stdout [END] ---");
    }
    log::trace!("--- cargo command stderr [BEGIN] ---");
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    log::trace!("--- cargo command stderr [END] ---");

    if !output.status.success() {
        std::process::exit(output.status.code().unwrap_or(1));
    }

    let build = extract_build_details(&cmd_output)?;

    if is_build || is_run || is_test {
        log::debug!("searching for WebAssembly modules to componentize");
        for (wasm, package_name, fresh) in build.wasms.iter() {
            if wasm.exists() {
                for PackageComponentMetadata { package, metadata } in packages {
                    if package_name == &package.name {
                        if let Some(metadata) = &metadata {
                            let is_bin = is_test || package.targets.iter().any(|t| t.is_bin());

                            let parent = match wasm.parent() {
                                Some(folder) => {
                                    if folder.file_name().and_then(OsStr::to_str) == Some("deps") {
                                        folder.parent().unwrap_or(folder)
                                    } else {
                                        folder
                                    }
                                }
                                None => wasm.as_path(),
                            };
                            let basename = wasm
                                .file_name()
                                .ok_or_else(|| anyhow::anyhow!("failed to get wasm file name"))?;

                            // Cargo will always overwrite our `wasm` above with its own internal
                            // cache. It's internal cache largely uses hard links.
                            //
                            // If `fresh` is *false*, then Cargo just built `wasm` and we need to
                            // process it. If `fresh` is *true*, then we may have previously
                            // processed it. If our previous processing was successful the output
                            // was placed at `*.component.wasm`, so we use that to overwrite the
                            // `*.wasm` file. In the process we also create a `*.module.wasm` for
                            // debugging.
                            //
                            // Note that we remove files before renaming and such to ensure that
                            // we're not accidentally updating the wrong hard link and such.
                            let temporary_module =
                                parent.join(basename).with_extension("module.wasm");
                            let temporary_component =
                                parent.join(basename).with_extension("component.wasm");

                            drop(fs::remove_file(&temporary_module));
                            fs::rename(wasm, &temporary_module)?;
                            if !*fresh || !temporary_component.exists() {
                                fs::copy(&temporary_module, &temporary_component)?;
                                create_component(config, metadata, &temporary_component, is_bin)?;
                            }
                            drop(fs::remove_file(wasm));
                            fs::hard_link(&temporary_component, wasm)
                                .or_else(|_| fs::copy(&temporary_component, wasm).map(|_| ()))?;

                            outputs.push(temporary_component);
                        }
                    }
                }
            } else {
                log::debug!(
                    "unable to convert wasm file to component `{}`",
                    wasm.to_string_lossy(),
                );
            }
        }

        for PackageComponentMetadata {
            package,
            metadata: _,
        } in packages
        {
            if !build
                .wasms
                .iter()
                .any(|(_, package_id, _)| package_id.starts_with(&package.name))
            {
                log::debug!("no output found for package `{name}`", name = package.name);
            }
        }
    }

    if let Some(runner) = runner {
        for run in outputs.iter() {
            let mut cmd = Command::new(&runner.path);
            cmd.args(&runner.args)
                .arg("--")
                .arg(run)
                .args(runtime_args.iter().skip(1));
            log::debug!("spawning command {:?}", cmd);

            let output = cmd.output().context("failed to spawn wasi runner")?;
            log::trace!("--- wasi runner stdout [BEGIN] ---");
            println!("{}", String::from_utf8_lossy(&output.stdout));
            log::trace!("--- wasi runner stdout [END] ---");
            log::trace!("--- wasi runner stderr [BEGIN] ---");
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            log::trace!("--- wasi runner stderr [END] ---");
            log::debug!(
                "wasi runner status code: {}",
                output.status.code().unwrap_or(1)
            );
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
pub fn load_metadata(
    terminal: &Terminal,
    manifest_path: Option<&Path>,
    ignore_version_mismatch: bool,
) -> Result<Metadata> {
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

    let metadata = command.exec().context("failed to load cargo metadata")?;

    if !ignore_version_mismatch {
        let this_version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        for package in &metadata.packages {
            match package.dependencies.iter().find(|dep| {
                dep.rename.as_deref().unwrap_or(dep.name.as_str()) == BINDINGS_CRATE_NAME
            }) {
                Some(bindings_crate) => {
                    let s = bindings_crate.req.to_string();
                    match s.strip_prefix('^').unwrap_or(&s).parse::<Version>() {
                        Ok(v) => {
                            if this_version.major == v.major
                                && (this_version.major > 0 || this_version.minor == v.minor)
                            {
                                // Version should be compatible
                                continue;
                            }

                            if this_version.major > v.major
                                || (this_version.major == v.major && this_version.minor > v.minor)
                            {
                                // cargo-component is newer, so warn about upgrading `Cargo.toml`
                                terminal.warn(format!("manifest `{path}` uses an older version of `{BINDINGS_CRATE_NAME}` ({v}) than cargo-component ({this_version}); use `cargo component upgrade --no-install` to update the manifest", path = package.manifest_path))?;
                            } else {
                                // cargo-component itself is out of date; warn to upgrade
                                terminal.warn(format!("manifest `{path}` uses a newer version of `{BINDINGS_CRATE_NAME}` ({v}) than cargo-component ({this_version}); use `cargo component upgrade` to upgrade to the latest version", path = package.manifest_path))?;
                            };
                        }
                        _ => continue,
                    }
                }
                None => continue,
            }
        }
    }

    Ok(metadata)
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

        generate_package_bindings(
            config,
            resolution,
            bindings_dir.as_std_path(),
            last_modified_exe,
        )
        .await?;
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
    bindings_dir: &Path,
    last_modified_exe: SystemTime,
) -> Result<()> {
    let output_dir = bindings_dir.join(&resolution.metadata.name);
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

fn adapter_bytes(metadata: &ComponentMetadata, binary: bool) -> Result<Cow<[u8]>> {
    if let Some(adapter) = &metadata.section.adapter {
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
        Ok(Cow::Borrowed(include_bytes!(concat!(
            "../adapters/",
            env!("WASI_ADAPTER_VERSION"),
            "/wasi_snapshot_preview1.command.wasm"
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
) -> Result<()> {
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
        .module(&module)?
        .adapter("wasi_snapshot_preview1", &adapter_bytes(metadata, binary)?)
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

/// Represents known cargo build outputs.
#[derive(Default, Debug)]
struct CargoBuild {
    /// The `*.wasm` artifacts we found during this build, in addition to the
    /// profile that they were built with and whether or not it was `fresh`
    /// during this build.
    wasms: Vec<(PathBuf, String, bool)>,
}

fn extract_build_details(json: &str) -> Result<CargoBuild> {
    let mut build = CargoBuild::default();
    if json.is_empty() {
        return Ok(build);
    }
    let reader = std::io::BufReader::new(json.as_bytes()); //command.stdout.take().unwrap());
    for message in Message::parse_stream(reader) {
        if let Message::CompilerArtifact(artifact) = message? {
            for file in artifact.filenames {
                let file = PathBuf::from(file);
                if file.extension().and_then(|s| s.to_str()) == Some("wasm") {
                    build
                        .wasms
                        .push((file, artifact.target.name.clone(), artifact.fresh));
                }
            }
        }
    }
    Ok(build)
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
