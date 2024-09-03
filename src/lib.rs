//! Cargo support for WebAssembly components.

#![deny(missing_docs)]

use std::{
    borrow::Cow,
    collections::HashMap,
    env,
    fmt::{self, Write},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::SystemTime,
};

use anyhow::{bail, Context, Result};
use bindings::BindingsGenerator;
use cargo_component_core::{
    lock::{LockFile, LockFileResolver, LockedPackage, LockedPackageVersion},
    terminal::Colors,
};
use cargo_config2::{PathAndArgs, TargetTripleRef};
use cargo_metadata::{Artifact, Message, Metadata, MetadataCommand, Package};
use semver::Version;
use shell_escape::escape;
use tempfile::NamedTempFile;
use wasm_metadata::{Link, LinkType, RegistryMetadata};
use wasm_pkg_client::{
    caching::{CachingClient, FileCache},
    PackageRef, PublishOpts, Registry,
};
use wasmparser::{Parser, Payload};
use wit_component::ComponentEncoder;

use crate::target::install_wasm32_wasip1;

use config::{CargoArguments, CargoPackageSpec, Config};
use lock::{acquire_lock_file_ro, acquire_lock_file_rw};
use metadata::ComponentMetadata;
use registry::{PackageDependencyResolution, PackageResolutionMap};

mod bindings;
pub mod commands;
pub mod config;
mod generator;
mod lock;
mod metadata;
mod registry;
mod target;

fn is_wasm_target(target: &str) -> bool {
    target == "wasm32-wasi" || target == "wasm32-wasip1" || target == "wasm32-unknown-unknown"
}

/// Represents a cargo package paired with its component metadata.
#[derive(Debug)]
pub struct PackageComponentMetadata<'a> {
    /// The cargo package.
    pub package: &'a Package,
    /// The associated component metadata.
    pub metadata: ComponentMetadata,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum CargoCommand {
    #[default]
    Other,
    Help,
    Build,
    Run,
    Test,
    Bench,
    Serve,
}

impl CargoCommand {
    fn buildable(self) -> bool {
        matches!(
            self,
            Self::Build | Self::Run | Self::Test | Self::Bench | Self::Serve
        )
    }

    fn runnable(self) -> bool {
        matches!(self, Self::Run | Self::Test | Self::Bench | Self::Serve)
    }

    fn testable(self) -> bool {
        matches!(self, Self::Test | Self::Bench)
    }
}

impl fmt::Display for CargoCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Help => write!(f, "help"),
            Self::Build => write!(f, "build"),
            Self::Run => write!(f, "run"),
            Self::Test => write!(f, "test"),
            Self::Bench => write!(f, "bench"),
            Self::Serve => write!(f, "serve"),
            Self::Other => write!(f, "<unknown>"),
        }
    }
}

impl From<&str> for CargoCommand {
    fn from(s: &str) -> Self {
        match s {
            "h" | "help" => Self::Help,
            "b" | "build" | "rustc" => Self::Build,
            "r" | "run" => Self::Run,
            "t" | "test" => Self::Test,
            "bench" => Self::Bench,
            "serve" => Self::Serve,
            _ => Self::Other,
        }
    }
}

/// Runs the cargo command as specified in the configuration.
///
/// Note: if the command returns a non-zero status, or if the
/// `--help` option was given on the command line, this
/// function will exit the process.
///
/// Returns any relevant output components.
pub async fn run_cargo_command(
    client: Arc<CachingClient<FileCache>>,
    config: &Config,
    metadata: &Metadata,
    packages: &[PackageComponentMetadata<'_>],
    subcommand: Option<&str>,
    cargo_args: &CargoArguments,
    spawn_args: &[String],
) -> Result<Vec<PathBuf>> {
    let import_name_map = generate_bindings(client, config, metadata, packages, cargo_args).await?;

    let cargo_path = std::env::var("CARGO")
        .map(PathBuf::from)
        .ok()
        .unwrap_or_else(|| PathBuf::from("cargo"));

    let command = if cargo_args.help {
        // Treat `--help` as the help command
        CargoCommand::Help
    } else {
        subcommand.map(CargoCommand::from).unwrap_or_default()
    };

    let (build_args, output_args) = match spawn_args.iter().position(|a| a == "--") {
        Some(position) => spawn_args.split_at(position),
        None => (spawn_args, &[] as _),
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
        "spawning cargo `{path}` with arguments `{args:?}`",
        path = cargo_path.display(),
        args = args.clone().collect::<Vec<_>>(),
    );

    let mut cargo = Command::new(&cargo_path);
    if matches!(command, CargoCommand::Run | CargoCommand::Serve) {
        // Treat run and serve as build commands as we need to componentize the output
        cargo.arg("build");
        if let Some(arg) = args.peek() {
            if Some((*arg).as_str()) == subcommand {
                args.next().unwrap();
            }
        }
    }
    cargo.args(args);

    let cargo_config = cargo_config2::Config::load()?;

    // Handle the target for buildable commands
    if command.buildable() {
        install_wasm32_wasip1(config)?;

        // Add an implicit wasm32-wasip1 target if there isn't a wasm target present
        if !cargo_args.targets.iter().any(|t| is_wasm_target(t))
            && !cargo_config
                .build
                .target
                .as_ref()
                .is_some_and(|v| v.iter().any(|t| is_wasm_target(t.triple())))
        {
            cargo.arg("--target").arg("wasm32-wasip1");
        }

        if let Some(format) = &cargo_args.message_format {
            if format != "json-render-diagnostics" {
                bail!("unsupported cargo message format `{format}`");
            }
        }

        // It will output the message as json so we can extract the wasm files
        // that will be componentized
        cargo.arg("--message-format").arg("json-render-diagnostics");
        cargo.stdout(Stdio::piped());
    } else {
        cargo.stdout(Stdio::inherit());
    }

    // At this point, spawn the command for help and terminate
    if command == CargoCommand::Help {
        let mut child = cargo.spawn().context(format!(
            "failed to spawn `{path}`",
            path = cargo_path.display()
        ))?;

        let status = child.wait().context(format!(
            "failed to wait for `{path}` to finish",
            path = cargo_path.display()
        ))?;

        std::process::exit(status.code().unwrap_or(0));
    }

    if needs_runner && command.testable() {
        // Only build for the test target; running will be handled
        // after the componentization
        cargo.arg("--no-run");
    }

    let runner = if needs_runner && command.runnable() {
        Some(get_runner(&cargo_config, command == CargoCommand::Serve)?)
    } else {
        None
    };

    let artifacts = spawn_cargo(cargo, &cargo_path, cargo_args, command.buildable())?;

    let outputs = componentize_artifacts(
        config,
        metadata,
        &artifacts,
        packages,
        &import_name_map,
        command,
        output_args,
    )?;

    if let Some(runner) = runner {
        spawn_outputs(config, &runner, output_args, &outputs, command)?;
    }

    Ok(outputs.into_iter().map(|o| o.path).collect())
}

fn get_runner(cargo_config: &cargo_config2::Config, serve: bool) -> Result<PathAndArgs> {
    // We check here before we actually build that a runtime is present.
    // We first check the runner for `wasm32-wasip1` in the order from
    // cargo's convention for a user-supplied runtime (path or executable)
    // and use the default, namely `wasmtime`, if it is not set.
    let (runner, using_default) = cargo_config
        .runner(TargetTripleRef::from("wasm32-wasip1"))
        .unwrap_or_default()
        .map(|runner_override| (runner_override, false))
        .unwrap_or_else(|| {
            (
                PathAndArgs::new("wasmtime")
                    .args(if serve {
                        vec!["serve", "-S", "cli", "-S", "http"]
                    } else {
                        vec!["-S", "preview2", "-S", "cli"]
                    })
                    .to_owned(),
                true,
            )
        });

    // Treat the runner object as an executable with list of arguments it
    // that was extracted by splitting each whitespace. This allows the user
    // to provide arguments which are passed to wasmtime without having to
    // add more command-line argument parsing to this crate.
    let wasi_runner = runner.path.to_string_lossy().into_owned();

    if !using_default {
        // check if the override runner exists
        if !(runner.path.exists() || which::which(&runner.path).is_ok()) {
            bail!(
                "failed to find `{wasi_runner}` specified by either the `CARGO_TARGET_WASM32_WASIP1_RUNNER`\
                environment variable or as the `wasm32-wasip1` runner in `.cargo/config.toml`"
            );
        }
    } else if which::which(&runner.path).is_err() {
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

    Ok(runner)
}

fn spawn_cargo(
    mut cmd: Command,
    cargo: &Path,
    cargo_args: &CargoArguments,
    process_messages: bool,
) -> Result<Vec<Artifact>> {
    log::debug!("spawning command {:?}", cmd);

    let mut child = cmd.spawn().context(format!(
        "failed to spawn `{cargo}`",
        cargo = cargo.display()
    ))?;

    let mut artifacts = Vec::new();
    if process_messages {
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
                    for path in &artifact.filenames {
                        match path.extension() {
                            Some("wasm") => {
                                artifacts.push(artifact);
                                break;
                            }
                            _ => continue,
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

    Ok(artifacts)
}

struct Output {
    /// The path to the output.
    path: PathBuf,
    /// The display name if the output is an executable.
    display: Option<String>,
}

fn componentize_artifacts(
    config: &Config,
    cargo_metadata: &Metadata,
    artifacts: &[Artifact],
    packages: &[PackageComponentMetadata<'_>],
    import_name_map: &HashMap<String, HashMap<String, String>>,
    command: CargoCommand,
    output_args: &[String],
) -> Result<Vec<Output>> {
    let mut outputs = Vec::new();
    let cwd =
        env::current_dir().with_context(|| "couldn't get the current directory of the process")?;

    // Acquire the lock file to ensure any other cargo-component process waits for this to complete
    let _file_lock = acquire_lock_file_ro(config.terminal(), cargo_metadata)?;

    for artifact in artifacts {
        for path in artifact
            .filenames
            .iter()
            .filter(|p| p.extension() == Some("wasm") && p.exists())
        {
            let (package, metadata) = match packages
                .iter()
                .find(|p| p.package.id == artifact.package_id)
            {
                Some(PackageComponentMetadata { package, metadata }) => (package, metadata),
                _ => continue,
            };

            match read_artifact(path.as_std_path(), metadata.section_present)? {
                ArtifactKind::Module => {
                    log::debug!(
                        "output file `{path}` is a WebAssembly module that will not be componentized"
                    );
                    continue;
                }
                ArtifactKind::Componentizable(bytes) => {
                    componentize(
                        config,
                        (cargo_metadata, metadata),
                        import_name_map
                            .get(&package.name)
                            .expect("package already processed"),
                        artifact,
                        path.as_std_path(),
                        &cwd,
                        &bytes,
                    )?;
                }
                ArtifactKind::Component => {
                    log::debug!("output file `{path}` is already a WebAssembly component");
                }
                ArtifactKind::Other => {
                    log::debug!("output file `{path}` is not a WebAssembly module or component");
                    continue;
                }
            }

            let mut output = Output {
                path: path.as_std_path().into(),
                display: None,
            };

            if command.testable() && artifact.profile.test
                || (matches!(command, CargoCommand::Run | CargoCommand::Serve)
                    && !artifact.profile.test)
            {
                output.display = Some(output_display_name(
                    cargo_metadata,
                    artifact,
                    path.as_std_path(),
                    &cwd,
                    command,
                    output_args,
                ));
            }

            outputs.push(output);
        }
    }

    Ok(outputs)
}

fn output_display_name(
    metadata: &Metadata,
    artifact: &Artifact,
    path: &Path,
    cwd: &Path,
    command: CargoCommand,
    output_args: &[String],
) -> String {
    // The format of the display name is intentionally the same
    // as what `cargo` formats for running executables.
    let test_path = &artifact.target.src_path;
    let short_test_path = test_path
        .strip_prefix(&metadata.workspace_root)
        .unwrap_or(test_path);

    if artifact.target.is_test() || artifact.target.is_bench() {
        format!(
            "{short_test_path} ({path})",
            path = path.strip_prefix(cwd).unwrap_or(path).display()
        )
    } else if command == CargoCommand::Test {
        format!(
            "unittests {short_test_path} ({path})",
            path = path.strip_prefix(cwd).unwrap_or(path).display()
        )
    } else if command == CargoCommand::Bench {
        format!(
            "benches {short_test_path} ({path})",
            path = path.strip_prefix(cwd).unwrap_or(path).display()
        )
    } else {
        let mut s = String::new();
        write!(&mut s, "`").unwrap();

        write!(
            &mut s,
            "{}",
            path.strip_prefix(cwd).unwrap_or(path).display()
        )
        .unwrap();

        for arg in output_args.iter().skip(1) {
            write!(&mut s, " {}", escape(arg.into())).unwrap();
        }

        write!(&mut s, "`").unwrap();
        s
    }
}

fn spawn_outputs(
    config: &Config,
    runner: &PathAndArgs,
    output_args: &[String],
    outputs: &[Output],
    command: CargoCommand,
) -> Result<()> {
    let executables = outputs
        .iter()
        .filter_map(|output| {
            output
                .display
                .as_ref()
                .map(|display| (display, &output.path))
        })
        .collect::<Vec<_>>();

    if matches!(command, CargoCommand::Run | CargoCommand::Serve) && executables.len() > 1 {
        config.terminal().error(
            "`cargo component {command}` can run at most one component, but multiple were specified",
        )
    } else if executables.is_empty() {
        config.terminal().error(format!(
            "a component {ty} target must be available for `cargo component {command}`",
            ty = if matches!(command, CargoCommand::Run | CargoCommand::Serve) {
                "bin"
            } else {
                "test"
            }
        ))
    } else {
        for (display, executable) in executables {
            config.terminal().status("Running", display)?;

            let mut cmd = Command::new(&runner.path);
            cmd.args(&runner.args)
                .arg("--")
                .arg(executable)
                .args(output_args.iter().skip(1))
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

        Ok(())
    }
}

enum ArtifactKind {
    /// A WebAssembly module that will not be componentized.
    Module,
    /// A WebAssembly module that will be componentized.
    Componentizable(Vec<u8>),
    /// A WebAssembly component.
    Component,
    /// An artifact that is not a WebAssembly module or component.
    Other,
}

fn read_artifact(path: &Path, mut componentizable: bool) -> Result<ArtifactKind> {
    let mut file = File::open(path).with_context(|| {
        format!(
            "failed to open build output `{path}`",
            path = path.display()
        )
    })?;

    let mut header = [0; 8];
    if file.read_exact(&mut header).is_err() {
        return Ok(ArtifactKind::Other);
    }

    if Parser::is_core_wasm(&header) {
        file.seek(SeekFrom::Start(0)).with_context(|| {
            format!(
                "failed to seek to the start of `{path}`",
                path = path.display()
            )
        })?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).with_context(|| {
            format!(
                "failed to read output WebAssembly module `{path}`",
                path = path.display()
            )
        })?;

        if !componentizable {
            let parser = Parser::new(0);
            for payload in parser.parse_all(&bytes) {
                if let Payload::CustomSection(reader) = payload.with_context(|| {
                    format!(
                        "failed to parse output WebAssembly module `{path}`",
                        path = path.display()
                    )
                })? {
                    if reader.name().starts_with("component-type") {
                        componentizable = true;
                        break;
                    }
                }
            }
        }

        if componentizable {
            Ok(ArtifactKind::Componentizable(bytes))
        } else {
            Ok(ArtifactKind::Module)
        }
    } else if Parser::is_component(&header) {
        Ok(ArtifactKind::Component)
    } else {
        Ok(ArtifactKind::Other)
    }
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
        metadata.workspace_default_packages()
    };

    pkgs.into_iter()
        .map(PackageComponentMetadata::new)
        .collect::<Result<_>>()
}

async fn generate_bindings(
    client: Arc<CachingClient<FileCache>>,
    config: &Config,
    metadata: &Metadata,
    packages: &[PackageComponentMetadata<'_>],
    cargo_args: &CargoArguments,
) -> Result<HashMap<String, HashMap<String, String>>> {
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

    let cwd =
        env::current_dir().with_context(|| "couldn't get the current directory of the process")?;

    let resolver = lock_file.as_ref().map(LockFileResolver::new);
    let resolution_map = create_resolution_map(client, packages, resolver).await?;
    let mut import_name_map = HashMap::new();
    for PackageComponentMetadata { package, .. } in packages {
        let resolution = resolution_map.get(&package.id).expect("missing resolution");
        import_name_map.insert(
            package.name.clone(),
            generate_package_bindings(config, resolution, &cwd).await?,
        );
    }

    // Update the lock file if it exists or if the new lock file is non-empty
    let new_lock_file = resolution_map.to_lock_file();
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

    Ok(import_name_map)
}

async fn create_resolution_map<'a>(
    client: Arc<CachingClient<FileCache>>,
    packages: &'a [PackageComponentMetadata<'_>],
    lock_file: Option<LockFileResolver<'_>>,
) -> Result<PackageResolutionMap<'a>> {
    let mut map = PackageResolutionMap::default();

    for PackageComponentMetadata { package, metadata } in packages {
        let resolution =
            PackageDependencyResolution::new(client.clone(), metadata, lock_file).await?;

        map.insert(package.id.clone(), resolution);
    }

    Ok(map)
}

async fn generate_package_bindings(
    config: &Config,
    resolution: &PackageDependencyResolution<'_>,
    cwd: &Path,
) -> Result<HashMap<String, String>> {
    if !resolution.metadata.section_present && resolution.metadata.target_path().is_none() {
        log::debug!(
            "skipping generating bindings for package `{name}`",
            name = resolution.metadata.name
        );
        return Ok(HashMap::new());
    }

    // If there is no wit files and no dependencies, stop generating the bindings file for it.
    let (generator, import_name_map) = match BindingsGenerator::new(resolution).await? {
        Some(v) => v,
        None => return Ok(HashMap::new()),
    };

    // TODO: make the output path configurable
    let output_dir = resolution
        .metadata
        .manifest_path
        .parent()
        .unwrap()
        .join("src");
    let bindings_path = output_dir.join("bindings.rs");

    config.terminal().status(
        "Generating",
        format!(
            "bindings for {name} ({path})",
            name = resolution.metadata.name,
            path = bindings_path
                .strip_prefix(cwd)
                .unwrap_or(&bindings_path)
                .display()
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

    Ok(import_name_map)
}

fn adapter_bytes(
    config: &Config,
    metadata: &ComponentMetadata,
    is_command: bool,
) -> Result<Cow<'static, [u8]>> {
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

    if is_command {
        if metadata.section.proxy {
            config
                .terminal()
                .warn("ignoring `proxy` setting in `Cargo.toml` for command component")?;
        }

        Ok(Cow::Borrowed(
            wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_COMMAND_ADAPTER,
        ))
    } else if metadata.section.proxy {
        Ok(Cow::Borrowed(
            wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_PROXY_ADAPTER,
        ))
    } else {
        Ok(Cow::Borrowed(
            wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_REACTOR_ADAPTER,
        ))
    }
}

fn componentize(
    config: &Config,
    (cargo_metadata, metadata): (&Metadata, &ComponentMetadata),
    import_name_map: &HashMap<String, String>,
    artifact: &Artifact,
    path: &Path,
    cwd: &Path,
    bytes: &[u8],
) -> Result<()> {
    let is_command =
        artifact.profile.test || artifact.target.crate_types.iter().any(|t| t == "bin");

    log::debug!(
        "componentizing WebAssembly module `{path}` as a {kind} component (fresh = {fresh})",
        path = path.display(),
        kind = if is_command { "command" } else { "reactor" },
        fresh = artifact.fresh,
    );

    // Only print the message if the artifact was not fresh
    // Due to the way cargo currently works on macOS, it will overwrite
    // a previously generated component on an up-to-date build.
    //
    // Therefore, we always componentize the artifact on macOS, but we
    // only print the status message if the artifact was not fresh.
    //
    // See: https://github.com/rust-lang/cargo/blob/99ad42deb4b0be0cdb062d333d5e63460a94c33c/crates/cargo-util/src/paths.rs#L542-L550
    if !artifact.fresh {
        config.terminal().status(
            "Creating",
            format!(
                "component {path}",
                path = path.strip_prefix(cwd).unwrap_or(path).display()
            ),
        )?;
    }

    let encoder = ComponentEncoder::default()
        .module(bytes)?
        .import_name_map(import_name_map.clone())
        .adapter(
            "wasi_snapshot_preview1",
            &adapter_bytes(config, metadata, is_command)?,
        )
        .with_context(|| {
            format!(
                "failed to load adapter module `{path}`",
                path = metadata
                    .section
                    .adapter
                    .as_deref()
                    .unwrap_or_else(|| Path::new("<built-in>"))
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

    // To make the write atomic, first write to a temp file and then rename the file
    let temp_dir = cargo_metadata.target_directory.join("tmp");
    fs::create_dir_all(&temp_dir)
        .with_context(|| format!("failed to create directory `{temp_dir}`"))?;

    let mut file = NamedTempFile::new_in(&temp_dir)
        .with_context(|| format!("failed to create temp file in `{temp_dir}`"))?;

    use std::io::Write;
    file.write_all(&component).with_context(|| {
        format!(
            "failed to write output component `{path}`",
            path = file.path().display()
        )
    })?;

    file.into_temp_path().persist(path).with_context(|| {
        format!(
            "failed to persist output component `{path}`",
            path = path.display()
        )
    })?;

    Ok(())
}

/// Represents options for a publish operation.
pub struct PublishOptions<'a> {
    /// The package to publish.
    pub package: &'a Package,
    /// The registry URL to publish to.
    pub registry: Option<&'a Registry>,
    /// The name of the package being published.
    pub name: &'a PackageRef,
    /// The version of the package being published.
    pub version: &'a Version,
    /// The path to the package being published.
    pub path: &'a Path,
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
pub async fn publish(
    config: &Config,
    client: Arc<CachingClient<FileCache>>,
    options: &PublishOptions<'_>,
) -> Result<()> {
    if options.dry_run {
        config
            .terminal()
            .warn("not publishing component to the registry due to the --dry-run option")?;
        return Ok(());
    }

    let bytes = fs::read(options.path).with_context(|| {
        format!(
            "failed to read component `{path}`",
            path = options.path.display()
        )
    })?;

    let bytes = add_registry_metadata(options.package, &bytes, options.path)?;

    config.terminal().status(
        "Publishing",
        format!("component {path}", path = options.path.display()),
    )?;

    let (name, version) = client
        .client()?
        .publish_release_data(
            Box::pin(std::io::Cursor::new(bytes)),
            PublishOpts {
                package: Some((options.name.to_owned(), options.version.to_owned())),
                registry: options.registry.cloned(),
            },
        )
        .await?;

    config
        .terminal()
        .status("Published", format!("package `{name}` v{version}"))?;

    Ok(())
}

/// Update the dependencies in the lock file.
///
/// This updates only `Cargo-component.lock`.
pub async fn update_lockfile(
    client: Arc<CachingClient<FileCache>>,
    config: &Config,
    metadata: &Metadata,
    packages: &[PackageComponentMetadata<'_>],
    lock_update_allowed: bool,
    locked: bool,
    dry_run: bool,
) -> Result<()> {
    // Read the current lock file and generate a new one
    let map = create_resolution_map(client, packages, None).await?;

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
                            "dependency `{name}` v{version}",
                            name = old_pkg.name,
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
                            "dependency `{name}` v{version}",
                            name = old_pkg.name,
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
                        "dependency `{name}` v{old} -> v{new}",
                        name = old_pkg.name,
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
                            "dependency `{name}` v{version}",
                            name = new_pkg.name,
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
                        "dependency `{name}` v{version}",
                        name = new_pkg.name,
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
