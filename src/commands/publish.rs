use crate::{
    commands::{workspace, CompileOptions},
    metadata::ComponentMetadata,
    registry,
    signing::get_signing_key,
    Config, PublishOptions,
};
use anyhow::{anyhow, bail, Context, Result};
use cargo::{core::compiler::CompileMode, ops::Packages};
use clap::{ArgAction, Args};
use semver::Version;
use std::path::PathBuf;
use url::Url;
use warg_crypto::signing::PrivateKey;
use warg_protocol::registry::PackageId;

/// Publish a package to a registry.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct PublishCommand {
    /// Do not print cargo log messages
    #[clap(long = "quiet", short = 'q')]
    pub quiet: bool,

    /// Use verbose output (-vv very verbose/build.rs output)
    #[clap(
        long = "verbose",
        short = 'v',
        action = ArgAction::Count
    )]
    pub verbose: u8,

    /// Allow dirty working directories to be published
    #[clap(long)]
    pub allow_dirty: bool,

    /// Coloring: auto, always, never
    #[clap(long = "color", value_name = "WHEN")]
    pub color: Option<String>,

    /// Build for the target triple (defaults to `wasm32-wasi`)
    #[clap(long = "target", value_name = "TRIPLE")]
    pub target: Option<String>,

    /// Require Cargo.lock and cache are up to date
    #[clap(long = "frozen")]
    pub frozen: bool,

    /// Directory for all generated artifacts
    #[clap(long = "target-dir", value_name = "DIRECTORY")]
    pub target_dir: Option<PathBuf>,

    /// Require Cargo.lock is up to date
    #[clap(long = "locked")]
    pub locked: bool,

    /// Cargo package to publish (see `cargo help pkgid`)
    #[clap(long = "package", short = 'p', value_name = "SPEC")]
    pub cargo_package: Option<String>,

    /// Path to Cargo.toml
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Run without accessing the network
    #[clap(long = "offline")]
    pub offline: bool,

    /// Space or comma separated list of features to activate
    #[clap(long = "features", value_name = "FEATURES")]
    pub features: Vec<String>,

    /// Activate all available features
    #[clap(long = "all-features")]
    pub all_features: bool,

    /// Unstable (nightly-only) flags to Cargo, see 'cargo -Z help' for details
    #[clap(long = "Z", value_name = "FLAG")]
    pub unstable_flags: Vec<String>,

    /// Do not activate the `default` feature
    #[clap(long = "no-default-features")]
    pub no_default_features: bool,

    /// Number of parallel jobs, defaults to # of CPUs
    #[clap(long = "jobs", short = 'j', value_name = "N")]
    pub jobs: Option<i32>,

    /// Do not abort the build as soon as there is an error (unstable)
    #[clap(long = "keep-going")]
    pub keep_going: bool,

    ///  Perform all checks without uploading
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// The key name to use for the signing key.
    #[clap(long, short, value_name = "KEY", default_value = "default")]
    pub key_name: String,

    /// The registry to publish to.
    #[clap(long = "registry", value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// Force generation of all dependency bindings.
    #[clap(long = "generate")]
    pub generate: bool,

    /// Initialize a new package in the registry.
    #[clap(long = "init")]
    pub init: bool,

    /// Override the id of the package being published.
    #[clap(long = "id", value_name = "PACKAGE")]
    pub id: Option<PackageId>,

    /// Overwrite the version of the package being published.
    #[clap(long = "version", value_name = "VERSION")]
    pub version: Option<Version>,

    /// If publishing a binary (i.e. WASI command), the name of the binary to publish.
    #[clap(long = "bin", value_name = "BIN")]
    pub bin: Option<String>,
}

impl PublishCommand {
    /// Executes the command.
    pub async fn exec(mut self, config: &mut Config) -> Result<()> {
        log::debug!("executing publish command");

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

        let ws = workspace(self.manifest_path.as_deref(), config)?;
        let package = if let Some(ref inner) = self.cargo_package {
            let pkg = Packages::from_flags(false, vec![], vec![inner.clone()])?;
            pkg.get_packages(&ws)?[0]
        } else {
            ws.current()?
        };

        let metadata = match ComponentMetadata::from_package(package)? {
            Some(metadata) => metadata,
            None => bail!(
                "manifest `{path}` is not a WebAssembly component package",
                path = package.manifest_path().display(),
            ),
        };

        let package_id: PackageId = self.id.take().or(metadata.section.package).ok_or_else(|| {
            anyhow!(
                "package id is not specified in manifest `{path}`; use the `--id` option to specify a package id",
                path = package.manifest_path().display(),
            )
        })?;

        let url = registry::find_url(
            config,
            self.registry.as_deref(),
            &metadata.section.registries,
        )?;

        let signing_key: PrivateKey = if let Ok(key) = std::env::var("CARGO_COMPONENT_PUBLISH_KEY")
        {
            key.parse().context("failed to parse signing key from `CARGO_COMPONENT_PUBLISH_KEY` environment variable")?
        } else {
            let url: Url = url
                .parse()
                .with_context(|| format!("failed to parse registry URL `{url}`"))?;

            get_signing_key(
                url.host_str()
                    .ok_or_else(|| anyhow!("registry URL `{url}` has no host"))?,
                &self.key_name,
            )?
        };

        let options = PublishOptions {
            force_generation: self.generate,
            id: &package_id,
            version: self.version.as_ref().unwrap_or(&metadata.version),
            compile_options: CompileOptions {
                workspace: false,
                exclude: Vec::new(),
                packages: self.cargo_package.into_iter().collect(),
                targets: self.target.into_iter().collect(),
                jobs: self.jobs,
                message_format: None,
                release: true,
                features: self.features,
                all_features: self.all_features,
                no_default_features: self.no_default_features,
                lib: self.bin.is_none(),
                all_targets: false,
                keep_going: self.keep_going,
                bins: self.bin.into_iter().collect(),
            }
            .into_cargo_options(config, CompileMode::Build)?,
            url,
            init: self.init,
            dry_run: self.dry_run,
            signing_key,
        };

        crate::publish(config, ws, &options).await
    }
}
