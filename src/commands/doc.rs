use crate::{
    commands::{workspace, CompileOptions, DocOptions},
    Config,
};
use anyhow::Result;
use cargo::core::compiler::CompileMode;
use clap::{ArgAction, Args};
use std::path::PathBuf;

/// Generate API documentation for a WebAssembly component API.
#[derive(Args)]
pub struct DocCommand {
    /// Opens the docs in a browser after the operation
    #[clap(long = "open")]
    pub open_result: bool,

    /// Don't build documentation for dependencies
    #[clap(long = "no-deps")]
    pub no_deps: bool,

    /// Do not print cargo log messages
    #[clap(long = "quiet", short = 'q')]
    pub quiet: bool,

    /// Package to document (see `cargo help pkgid`)
    #[clap(long = "package", short = 'p', value_name = "SPEC")]
    pub packages: Vec<String>,

    /// Document all packages in the workspace
    #[clap(long = "workspace", alias = "all")]
    pub workspace: bool,

    /// Exclude packages from the documentation
    #[clap(long = "exclude", value_name = "SPEC")]
    pub exclude: Vec<String>,

    /// Number of parallel jobs, defaults to # of CPUs
    #[clap(long = "jobs", short = 'j', value_name = "N")]
    pub jobs: Option<i32>,

    /// Document only this package's library
    #[clap(long = "lib")]
    pub lib: bool,

    /// Document artifacts in release mode, with optimizations
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

    /// Document for the target triple (defaults to `wasm32-wasi`)
    #[clap(long = "target", value_name = "TRIPLE")]
    pub targets: Vec<String>,

    /// Document all targets
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
        action = ArgAction::Count
    )]
    pub verbose: u8,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

    /// Require Cargo.lock and cache are up to date
    #[clap(long = "frozen")]
    pub frozen: bool,

    /// Do not abort the build as soon as there is an error (unstable)
    #[clap(long = "keep-going")]
    pub keep_going: bool,

    /// Require Cargo.lock is up to date
    #[clap(long = "locked")]
    pub locked: bool,

    /// Run without accessing the network
    #[clap(long = "offline")]
    pub offline: bool,

    /// Error format
    #[clap(long = "message-format", value_name = "FMT")]
    pub message_format: Option<String>,

    /// Unstable (nightly-only) flags to Cargo, see 'cargo -Z help' for details
    #[clap(long = "Z", value_name = "FLAG")]
    pub unstable_flags: Vec<String>,

    /// Force generation of all dependency bindings.
    #[clap(long = "generate")]
    pub generate: bool,
}

impl DocCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing document command");

        config.cargo_mut().configure(
            u32::from(self.verbose),
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
        let no_deps = self.no_deps;
        let workspace = workspace(self.manifest_path.as_deref(), config)?;
        let options = DocOptions::from(self)
            .into_cargo_options(config, CompileMode::Doc { deps: !no_deps })?;

        crate::doc(config, workspace, &options, force_generation).await
    }
}

impl From<DocCommand> for DocOptions {
    fn from(cmd: DocCommand) -> Self {
        DocOptions {
            open_result: cmd.open_result,
            compile_opts: CompileOptions {
                workspace: cmd.workspace,
                exclude: cmd.exclude,
                packages: cmd.packages,
                targets: cmd.targets,
                jobs: cmd.jobs,
                message_format: cmd.message_format,
                release: cmd.release,
                features: cmd.features,
                all_features: cmd.all_features,
                no_default_features: cmd.no_default_features,
                lib: cmd.lib,
                all_targets: cmd.all_targets,
                keep_going: cmd.keep_going,
                bins: vec![],
            },
        }
    }
}
