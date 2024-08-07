//! Module for common command implementation.
use std::path::PathBuf;

use clap::{ArgAction, Args};

use crate::terminal::{Color, Terminal, Verbosity};

/// The environment variable name for setting a cache directory location
pub const CACHE_DIR_ENV_VAR: &str = "CARGO_COMPONENT_CACHE_DIR";
/// The environment variable name for setting a path to a config file
pub const CONFIG_FILE_ENV_VAR: &str = "CARGO_COMPONENT_CONFIG_FILE";

/// Common options for commands.
#[derive(Args)]
#[command(
    after_help = "Unrecognized subcommands will be passed to cargo verbatim after relevant component bindings are updated."
)]
pub struct CommonOptions {
    /// Do not print log messages
    #[clap(long = "quiet", short = 'q')]
    pub quiet: bool,

    /// Use verbose output (-vv very verbose output)
    #[clap(
        long = "verbose",
        short = 'v',
        action = ArgAction::Count
    )]
    pub verbose: u8,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<Color>,

    /// The path to the cache directory to store component dependencies.
    #[clap(long = "cache-dir", env = CACHE_DIR_ENV_VAR)]
    pub cache_dir: Option<PathBuf>,

    /// The path to the pkg-tools config file
    #[clap(long = "config", env = CONFIG_FILE_ENV_VAR)]
    pub config: Option<PathBuf>,
}

impl CommonOptions {
    /// Creates a new terminal from the common options.
    pub fn new_terminal(&self) -> Terminal {
        Terminal::new(
            if self.quiet {
                Verbosity::Quiet
            } else {
                match self.verbose {
                    0 => Verbosity::Normal,
                    _ => Verbosity::Verbose,
                }
            },
            self.color.unwrap_or_default(),
        )
    }
}
