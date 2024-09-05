use std::{path::PathBuf, sync::Arc};

use anyhow::{bail, Context, Result};
use cargo_component_core::command::CommonOptions;
use clap::Args;
use wasm_pkg_client::{warg::WargRegistryConfig, Registry};

use crate::{
    config::{CargoArguments, CargoPackageSpec, Config},
    is_wasm_target, load_metadata, publish, run_cargo_command, PackageComponentMetadata,
    PublishOptions,
};

/// Publish a package to a registry.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct PublishCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Build for the target triple (defaults to `wasm32-wasip1`)
    #[clap(long = "target", value_name = "TRIPLE")]
    pub target: Option<String>,

    /// Require lock file and cache are up to date
    #[clap(long = "frozen")]
    pub frozen: bool,

    /// Directory for all generated artifacts
    #[clap(long = "target-dir", value_name = "DIRECTORY")]
    pub target_dir: Option<PathBuf>,

    /// Require lock file is up to date
    #[clap(long = "locked")]
    pub locked: bool,

    /// Cargo package to publish (see `cargo help pkgid`)
    #[clap(long = "package", short = 'p', value_name = "SPEC")]
    pub cargo_package: Option<CargoPackageSpec>,

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

    /// Do not activate the `default` feature
    #[clap(long = "no-default-features")]
    pub no_default_features: bool,

    /// Number of parallel jobs, defaults to # of CPUs
    #[clap(long = "jobs", short = 'j', value_name = "N")]
    pub jobs: Option<i32>,

    ///  Perform all checks without publishing
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// The registry to publish to.
    #[clap(long = "registry", value_name = "REGISTRY")]
    pub registry: Option<Registry>,
}

impl PublishCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing publish command");

        let mut config = Config::new(self.common.new_terminal(), self.common.config.clone())?;
        let client = config.client(self.common.cache_dir.clone(), false).await?;

        if let Some(target) = &self.target {
            if !is_wasm_target(target) {
                bail!("target `{}` is not a WebAssembly target", target);
            }
        }

        let metadata = load_metadata(self.manifest_path.as_deref())?;
        let spec = match &self.cargo_package {
            Some(spec) => Some(spec.clone()),
            None => CargoPackageSpec::find_current_package_spec(&metadata),
        };
        let packages = [PackageComponentMetadata::new(if let Some(spec) = &spec {
            metadata
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
                })?
        } else {
            metadata
                .root_package()
                .context("no root package found in manifest")?
        })?];

        let package = packages[0].package;
        let component_metadata = &packages[0].metadata;

        let name = component_metadata.section.package.as_ref().with_context(|| {
            format!(
                "package `{name}` is missing a `package.metadata.component.package` setting in manifest `{path}`",
                name = package.name,
                path = package.manifest_path
            )
        })?;

        if let Ok(key) = std::env::var("CARGO_COMPONENT_PUBLISH_KEY") {
            let registry = config.pkg_config.resolve_registry(name).ok_or_else(|| anyhow::anyhow!("Tried to set a signing key, but registry was not set and no default registry was found. Try setting the `--registry` option."))?.to_owned();
            // NOTE(thomastaylor312): If config doesn't already exist, this will essentially force warg
            // usage because we'll be creating a config for warg, which means it will default to that
            // protocol. So for all intents and purposes, setting a publish key forces warg usage.
            let reg_config = config
                .pkg_config
                .get_or_insert_registry_config_mut(&registry);
            let mut warg_conf = WargRegistryConfig::try_from(&*reg_config).unwrap_or_default();
            warg_conf.signing_key = Some(Arc::new(
                key.try_into().context("Failed to parse signing key")?,
            ));
            reg_config.set_backend_config("warg", warg_conf)?;
        }

        let cargo_build_args = CargoArguments {
            color: self.common.color,
            verbose: self.common.verbose as usize,
            help: false,
            quiet: self.common.quiet,
            targets: self.target.clone().into_iter().collect(),
            manifest_path: self.manifest_path.clone(),
            message_format: None,
            frozen: self.frozen,
            locked: self.locked,
            release: true,
            offline: self.offline,
            workspace: false,
            packages: self.cargo_package.clone().into_iter().collect(),
        };

        let spawn_args = self.build_args()?;
        let outputs = run_cargo_command(
            client.clone(),
            &config,
            &metadata,
            &packages,
            Some("build"),
            &cargo_build_args,
            &spawn_args,
        )
        .await?;
        if outputs.len() != 1 {
            bail!(
                "expected one output from `cargo build`, got {len}",
                len = outputs.len()
            );
        }

        let options = PublishOptions {
            package,
            name,
            registry: self.registry.as_ref(),
            version: &component_metadata.version,
            path: &outputs[0],
            dry_run: self.dry_run,
        };

        publish(&config, client, &options).await
    }

    fn build_args(&self) -> Result<Vec<String>> {
        let mut args = Vec::new();
        args.push("build".to_string());
        args.push("--release".to_string());

        if self.common.quiet {
            args.push("-q".to_string());
        }

        args.extend(
            std::iter::repeat("-v")
                .take(self.common.verbose as usize)
                .map(ToString::to_string),
        );

        if let Some(color) = self.common.color {
            args.push("--color".to_string());
            args.push(color.to_string());
        }

        if let Some(target) = &self.target {
            args.push("--target".to_string());
            args.push(target.clone());
        }

        if self.frozen {
            args.push("--frozen".to_string());
        }

        if let Some(target_dir) = &self.target_dir {
            args.push("--target-dir".to_string());
            args.push(
                target_dir
                    .as_os_str()
                    .to_str()
                    .with_context(|| {
                        format!(
                            "target directory `{dir}` is not valid UTF-8",
                            dir = target_dir.display()
                        )
                    })?
                    .to_string(),
            );
        }

        if self.locked {
            args.push("--locked".to_string());
        }

        if let Some(spec) = &self.cargo_package {
            args.push("--package".to_string());
            args.push(spec.to_string());
        }

        if let Some(manifest_path) = &self.manifest_path {
            args.push("--manifest-path".to_string());
            args.push(
                manifest_path
                    .as_os_str()
                    .to_str()
                    .with_context(|| {
                        format!(
                            "manifest path `{path}` is not valid UTF-8",
                            path = manifest_path.display()
                        )
                    })?
                    .to_string(),
            );
        }

        if self.offline {
            args.push("--offline".to_string());
        }

        if !self.features.is_empty() {
            args.push("--features".to_string());
            args.push(self.features.join(","));
        }

        if self.all_features {
            args.push("--all-features".to_string());
        }

        if self.no_default_features {
            args.push("--no-default-features".to_string());
        }

        if let Some(jobs) = self.jobs {
            args.push("--jobs".to_string());
            args.push(jobs.to_string());
        }

        Ok(args)
    }
}
