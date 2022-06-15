//! Commands for the `cargo-component` CLI.

use anyhow::{bail, Result};
use cargo::core::compiler::{BuildConfig, CompileMode, MessageFormat};
use cargo::core::resolver::CliFeatures;
use cargo::ops::{CompileFilter, Packages};
use cargo::util::interning::InternedString;
use cargo::{core::Workspace, util::important_paths::find_root_manifest_for_wd, Config};
use cargo_util::paths::normalize_path;
use std::path::{Path, PathBuf};

mod add;
mod build;
mod check;
mod clippy;
mod metadata;
mod new;

pub use self::add::*;
pub use self::build::*;
pub use self::check::*;
pub use self::clippy::*;
pub use self::metadata::*;
pub use self::new::*;
use crate::target;

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

fn message_format(option: Option<&str>) -> Result<MessageFormat> {
    let default_json = MessageFormat::Json {
        short: false,
        ansi: false,
        render_diagnostics: false,
    };

    let mut message_format = None;

    if let Some(option) = option {
        for fmt in option.split(',') {
            let fmt = fmt.to_ascii_lowercase();
            match fmt.as_str() {
                "json" => {
                    if message_format.is_some() {
                        bail!("cannot specify two kinds of `message-format` arguments");
                    }
                    message_format = Some(default_json);
                }
                "human" => {
                    if message_format.is_some() {
                        bail!("cannot specify two kinds of `message-format` arguments");
                    }
                    message_format = Some(MessageFormat::Human);
                }
                "short" => {
                    if message_format.is_some() {
                        bail!("cannot specify two kinds of `message-format` arguments");
                    }
                    message_format = Some(MessageFormat::Short);
                }
                "json-render-diagnostics" => {
                    if message_format.is_none() {
                        message_format = Some(default_json);
                    }
                    match &mut message_format {
                        Some(MessageFormat::Json {
                            render_diagnostics, ..
                        }) => *render_diagnostics = true,
                        _ => bail!("cannot specify two kinds of `message-format` arguments"),
                    }
                }
                "json-diagnostic-short" => {
                    if message_format.is_none() {
                        message_format = Some(default_json);
                    }
                    match &mut message_format {
                        Some(MessageFormat::Json { short, .. }) => *short = true,
                        _ => bail!("cannot specify two kinds of `message-format` arguments"),
                    }
                }
                "json-diagnostic-rendered-ansi" => {
                    if message_format.is_none() {
                        message_format = Some(default_json);
                    }
                    match &mut message_format {
                        Some(MessageFormat::Json { ansi, .. }) => *ansi = true,
                        _ => bail!("cannot specify two kinds of `message-format` arguments"),
                    }
                }
                s => bail!("invalid message format specifier: `{}`", s),
            }
        }
    }

    Ok(message_format.unwrap_or(MessageFormat::Human))
}

struct CompileOptions {
    workspace: bool,
    exclude: Vec<String>,
    packages: Vec<String>,
    targets: Vec<String>,
    jobs: Option<u32>,
    message_format: Option<String>,
    release: bool,
    features: Vec<String>,
    all_features: bool,
    no_default_features: bool,
    lib: bool,
    all_targets: bool,
    keep_going: bool,
}

impl CompileOptions {
    fn into_cargo_options(
        mut self,
        config: &Config,
        mode: CompileMode,
    ) -> Result<cargo::ops::CompileOptions> {
        let spec = Packages::from_flags(self.workspace, self.exclude, self.packages)?;

        if self.targets.is_empty() {
            target::install_wasm32_unknown_unknown()?;
            self.targets.push("wasm32-unknown-unknown".to_string());
        }

        log::debug!("using targets {:#?}", self.targets);

        let mut build_config =
            BuildConfig::new(config, self.jobs, self.keep_going, &self.targets, mode)?;

        build_config.message_format = message_format(self.message_format.as_deref())?;
        build_config.requested_profile =
            InternedString::new(if self.release { "release" } else { "dev" });

        Ok(cargo::ops::CompileOptions {
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
        })
    }
}
