//! Commands for the `cargo-component` CLI.

use anyhow::{bail, Result};
use cargo::{
    core::{
        compiler::{BuildConfig, CompileMode},
        resolver::CliFeatures,
        Workspace,
    },
    ops::{CompileFilter, CompileOptions, Packages},
    util::{important_paths::find_root_manifest_for_wd, interning::InternedString},
    Config,
};
use cargo_util::paths::normalize_path;
use clap::Args;
use std::path::{Path, PathBuf};

fn root_manifest(manifest_path: Option<&Path>, config: &Config) -> Result<PathBuf> {
    match manifest_path {
        Some(path) => {
            let normalized_path = normalize_path(path);
            if !normalized_path.ends_with("Cargo.toml") {
                bail!("the manifest-path must be a path to a Cargo.toml file")
            }
            if !normalized_path.exists() {
                bail!("manifest path `{}` does not exist", path.display())
            }
            Ok(normalized_path)
        }
        None => find_root_manifest_for_wd(config.cwd()),
    }
}

fn workspace<'a>(manifest_path: Option<&Path>, config: &'a Config) -> Result<Workspace<'a>> {
    let root = root_manifest(manifest_path, config)?;
    let mut ws = Workspace::new(&root, config)?;
    if config.cli_unstable().avoid_dev_deps {
        ws.set_require_optional_deps(false);
    }
    Ok(ws)
}

/// Compile a WebAssembly component and all of its dependencies
#[derive(Args)]
#[clap(name = "build")]
pub struct BuildCommand {
    /// Do not print cargo log messages
    #[clap(long = "quiet", short = 'q')]
    pub quiet: bool,

    /// Package to build (see `cargo help pkgid`)
    #[clap(long = "package", short = 'p', value_name = "SPEC")]
    pub packages: Vec<String>,

    /// Build all packages in the workspace
    #[clap(long = "workspace")]
    pub workspace: bool,

    /// Exclude packages from the build
    #[clap(long = "exclude", value_name = "SPEC")]
    pub exclude: Vec<String>,

    /// Number of parallel jobs, defaults to # of CPUs
    #[clap(long = "jobs", short = 'j', value_name = "N")]
    pub jobs: Option<u32>,

    /// Build only this package's library
    #[clap(long = "lib")]
    pub lib: bool,

    /// Build artifacts in release mode, with optimizations
    #[clap(long = "release", short = 'r')]
    pub release: bool,

    /// Space or comma separated list of features to activate
    #[clap(long = "features", value_name = "FEATURES")]
    pub features: Vec<String>,

    /// Activate all available features
    #[clap(long = "all-features")]
    pub all_features: bool,

    /// Do not activate the `default` feature
    #[clap(long = "no-default-features")]
    pub no_default_features: bool,

    /// Build for the target triple (defaults to `wasm32-unknown-unknown`)
    #[clap(long = "target", value_name = "TRIPLE")]
    pub targets: Vec<String>,

    /// Build all targets
    #[clap(long = "all-targets")]
    pub all_targets: bool,

    /// Directory for all generated artifacts
    #[clap(long = "target-dir", value_name = "DIRECTORY")]
    pub target_dir: Option<PathBuf>,

    /// Path to Cargo.toml
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Use verbose output (-vv very verbose/build.rs output)
    #[clap(
        long = "verbose",
        short = 'v',
        takes_value = false,
        parse(from_occurrences)
    )]
    pub verbose: u32,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

    /// Require Cargo.lock and cache are up to date
    #[clap(long = "frozen")]
    pub frozen: bool,

    /// Require Cargo.lock is up to date
    #[clap(long = "locked")]
    pub locked: bool,

    /// Run without accessing the network
    #[clap(long = "offline")]
    pub offline: bool,

    /// Unstable (nightly-only) flags to Cargo, see 'cargo -Z help' for details
    #[clap(long = "Z", value_name = "FLAG")]
    pub unstable_flags: Vec<String>,

    /// Force generation of all dependency bindings.
    #[clap(long = "generate")]
    pub generate: bool,
}

impl BuildCommand {
    /// Executes the command.
    pub fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing compile command");

        config.configure(
            self.verbose,
            self.quiet,
            self.color.as_deref(),
            self.frozen,
            self.locked,
            self.offline,
            &self.target_dir,
            &self.unstable_flags,
            &[],
        )?;

        let force_generation = self.generate;
        let workspace = workspace(self.manifest_path.as_deref(), config)?;
        let options = self.compile_options(config)?;

        crate::compile(config, workspace, &options, force_generation)
    }

    fn compile_options(mut self, config: &Config) -> Result<CompileOptions> {
        let spec = Packages::from_flags(self.workspace, self.exclude, self.packages)?;

        if self.targets.is_empty() {
            self.targets.push("wasm32-unknown-unknown".to_string());
        }

        log::debug!("compiling for targets {:#?}", self.targets);

        let mut build_config =
            BuildConfig::new(config, self.jobs, &self.targets, CompileMode::Build)?;

        build_config.requested_profile =
            InternedString::new(if self.release { "release" } else { "dev" });

        let opts = CompileOptions {
            build_config,
            cli_features: CliFeatures::from_command_line(
                &self.features,
                self.all_features,
                !self.no_default_features,
            )?,
            spec,
            filter: CompileFilter::from_raw_arguments(
                self.lib,
                // TODO: support bins/tests/examples/benches?
                Vec::new(),
                false,
                Vec::new(),
                false,
                Vec::new(),
                false,
                Vec::new(),
                false,
                self.all_targets,
            ),
            target_rustdoc_args: None,
            target_rustc_args: None,
            target_rustc_crate_types: None,
            local_rustdoc_args: None,
            rustdoc_document_private_items: false,
            honor_rust_version: true,
        };

        Ok(opts)
    }
}
