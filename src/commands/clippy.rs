use super::CheckCommand;
use crate::{
    commands::{workspace, CompileOptions},
    Config,
};
use anyhow::{anyhow, Result};
use cargo::core::compiler::CompileMode;
use cargo_util::paths::resolve_executable;
use clap::Args;
use std::{
    env,
    path::{Path, PathBuf},
};

/// Checks a package to catch common mistakes and improve your Rust code.
#[derive(Args)]
#[clap(
    after_help = r#"To allow or deny a lint from the command line you can use `cargo component clippy --`
with:

    -W --warn OPT       Set lint warnings
    -A --allow OPT      Set lint allowed
    -D --deny OPT       Set lint denied
    -F --forbid OPT     Set lint forbidden

You can use tool lints to allow or deny lints from your code, eg.:

    #[allow(clippy::needless_lifetimes)]"#
)]
pub struct ClippyCommand {
    /// Run Clippy only on the given crate, without linting the dependencies
    #[clap(long)]
    no_deps: bool,

    #[clap(flatten)]
    options: CheckCommand,

    /// Options to allow or deny a clippy lint
    #[clap(name = "OPTS", last = true, allow_hyphen_values = true)]
    clippy_options: Vec<String>,
}

impl ClippyCommand {
    /// Executes the command.
    pub async fn exec(self, config: &mut Config) -> Result<()> {
        log::debug!("executing clippy command");

        // Set the rustc wrapper to clippy's driver
        // This is the magic that turns `cargo check` into `cargo clippy`
        env::set_var("RUSTC_WORKSPACE_WRAPPER", Self::driver_path()?);

        // However, for cargo to respect that variable, we must recreate the config
        *config.cargo_mut() = cargo::Config::default()?;

        config.cargo_mut().configure(
            u32::from(self.options.verbose),
            self.options.quiet,
            self.options.color.as_deref(),
            self.options.frozen,
            self.options.locked,
            self.options.offline,
            &self.options.target_dir,
            &self.options.unstable_flags,
            &[],
        )?;

        let force_generation = self.options.generate;
        let workspace = workspace(self.options.manifest_path.as_deref(), config)?;
        let options = CompileOptions::from(self.options)
            .into_cargo_options(config, CompileMode::Check { test: false })?;

        // Clippy parses its args using a special delimiter
        let clippy_args: String = self
            .clippy_options
            .into_iter()
            .chain(self.no_deps.then(|| "--no-deps".to_string()))
            .map(|arg| format!("{arg}__CLIPPY_HACKERY__"))
            .collect();
        env::set_var("CLIPPY_ARGS", clippy_args);

        crate::check(config, workspace, &options, force_generation).await
    }

    fn driver_path() -> Result<PathBuf> {
        let mut path = env::current_exe()?.with_file_name("clippy-driver");

        if cfg!(windows) {
            path.set_extension("exe");
        }

        if path.is_file() {
            return Ok(path);
        }

        resolve_executable(Path::new("clippy-driver")).map_err(|_| {
            anyhow!("clippy driver was not found: run `rustup component add clippy` to install")
        })
    }
}
