use crate::{
    config::{CargoArguments, CargoPackageSpec, Config},
    is_wasm_target, load_metadata, publish, run_cargo_command, PackageComponentMetadata,
    PublishOptions,
};
use anyhow::{bail, Context, Result};
use cargo_component_core::{command::CommonOptions, keyring::get_signing_key, registry::find_url};
use clap::Args;
use std::path::PathBuf;
use warg_client::RegistryUrl;
use warg_crypto::signing::PrivateKey;

/// Publish a package to a registry.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct PublishCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Build for the target triple (defaults to `wasm32-wasi`)
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

    /// The key name to use for the signing key.
    #[clap(long, short, value_name = "KEY", default_value = "default")]
    pub key_name: String,

    /// The registry to publish to.
    #[clap(long = "registry", value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// Initialize a new package in the registry.
    #[clap(long = "init")]
    pub init: bool,
}

impl PublishCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing publish command");

        let config = Config::new(self.common.new_terminal())?;

        if let Some(target) = &self.target {
            if !is_wasm_target(target) {
                bail!("target `{}` is not a WebAssembly target", target);
            }
        }

        let metadata = load_metadata(config.terminal(), self.manifest_path.as_deref(), false)?;
        let packages = [PackageComponentMetadata::new(
            if let Some(spec) = &self.cargo_package {
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
            },
        )?];

        let package = packages[0].package;
        let component_metadata = packages[0].metadata.as_ref().with_context(|| {
            format!(
                "package `{name}` is missing component metadata in manifest `{path}`",
                name = package.name,
                path = package.manifest_path
            )
        })?;

        let id = component_metadata.section.package.as_ref().with_context(|| {
            format!(
                "package `{name}` is missing a `package.metadata.component.package` setting in manifest `{path}`",
                name = package.name,
                path = package.manifest_path
            )
        })?;

        let registry_url = find_url(
            self.registry.as_deref(),
            &component_metadata.section.registries,
            config.warg().default_url.as_deref(),
        )?;

        let signing_key = if let Ok(key) = std::env::var("CARGO_COMPONENT_PUBLISH_KEY") {
            PrivateKey::decode(key).context("failed to parse signing key from `CARGO_COMPONENT_PUBLISH_KEY` environment variable")?
        } else {
            let url: RegistryUrl = registry_url
                .parse()
                .with_context(|| format!("failed to parse registry URL `{registry_url}`"))?;

            get_signing_key(&url, &self.key_name)?
        };

        let cargo_build_args = CargoArguments {
            color: self.common.color,
            verbose: self.common.verbose as usize,
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
            registry_url,
            init: self.init,
            id,
            version: &component_metadata.version,
            path: &outputs[0],
            signing_key: &signing_key,
            dry_run: self.dry_run,
        };

        publish(&config, &options).await
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
