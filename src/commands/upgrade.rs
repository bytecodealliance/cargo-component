use crate::{load_metadata, Config, BINDINGS_CRATE_NAME};
use anyhow::{Context, Result};
use cargo_component_core::command::CommonOptions;
use cargo_metadata::Metadata;
use clap::Args;
use std::{fs, path::PathBuf};
use toml_edit::{value, Document};

/// Install the latest version of cargo-component and upgrade to the
/// corresponding version of cargo-component-bindings.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct UpgradeCommand {
    /// The common command options
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Don't actually write the Cargo.toml changes.
    ///
    /// Note that this will not prevent installing a new version of cargo-component itself;
    /// if you want to do that, you must also specify the '--no-install' flag.
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// Path to Cargo.toml
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Skip installing the latest version of cargo-component;
    /// instead just upgrade cargo-component-bindings to match
    /// the version currently running.
    #[clap(long = "no-install")]
    pub no_install: bool,
}

impl UpgradeCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing upgrade command");

        if !self.no_install {
            // Do the self-upgrade first, and then _unconditionally_ delegate
            // to whatever version of `cargo-component` is now at the same path as the
            // current executable.
            //
            // This avoids needing to query crates.io ourselves, scrape the version
            // from `cargo-component --version` etc.
            //
            // (We can't tell whether or not cargo-install actually installed anything
            // without scraping its output; it considers "already installed" as success.)
            //
            // Skip this in tests, but still delegate to a new instance of `cargo-component`
            // so that we can exercise as much of the flow as practicable.
            #[cfg(not(test))]
            upgrade_self()?;
            run_cargo_component_and_exit();
        }

        let config = Config::new(self.common.new_terminal())?;
        let metadata = load_metadata(config.terminal(), self.manifest_path.as_deref(), true)?;

        upgrade_bindings(&config, &metadata, self.dry_run).await?;

        Ok(())
    }
}

#[cfg(not(test))]
fn upgrade_self() -> Result<()> {
    log::debug!("running self-upgrade using cargo-install");

    let mut command = std::process::Command::new("cargo");
    command.args(["install", "cargo-component"]);

    match command.status() {
        Ok(status) => {
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
            Ok(())
        }
        Err(e) => {
            anyhow::bail!("failed to execute `cargo install` command: {e}")
        }
    }
}

fn run_cargo_component_and_exit() -> ! {
    log::debug!("running cargo-component from same path as this process");

    let mut args = std::env::args();

    // argv[0] cannot be relied up on as a path to the executable;
    // skip it and use `current_exe` instead.
    let _ = args.next();

    let mut command = std::process::Command::new(
        std::env::current_exe().expect("Failed to get path to current executable"),
    );
    command.args(args);

    // Unconditionally specify '--no-install' to prevent infinite recursion.
    command.arg("--no-install");

    match command.status() {
        Ok(status) => {
            std::process::exit(status.code().unwrap_or(1));
        }
        Err(e) => {
            log::error!("failed to delegate to `cargo-component` command: {e}");
            std::process::exit(1);
        }
    }
}

async fn upgrade_bindings(config: &Config, metadata: &Metadata, dry_run: bool) -> Result<()> {
    let self_version = semver::VersionReq::parse(env!("CARGO_PKG_VERSION"))
        .context("Failed to parse current cargo-component version")?;

    for package in metadata.workspace_packages() {
        let Some(bindings_dep) = package
            .dependencies
            .iter()
            .find(|dep| dep.name == "cargo-component-bindings")
        else {
            log::debug!(
                "Workspace package {} doesn't depend on cargo-component-bindings",
                package.name
            );
            continue;
        };

        if bindings_dep.req == self_version {
            config.terminal().status(
                "Skipping",
                format!(
                    "package `{}` as it already uses the current bindings crate version",
                    package.name
                ),
            )?;
            continue;
        }

        let manifest_path = package.manifest_path.as_std_path();
        let manifest = fs::read_to_string(manifest_path).with_context(|| {
            format!(
                "failed to read manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        let mut doc: Document = manifest.parse().with_context(|| {
            format!(
                "failed to parse manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        doc["dependencies"][BINDINGS_CRATE_NAME] = value(env!("CARGO_PKG_VERSION"));

        // Do this fairly late, so we exercise as much of the real code as possible
        // (encounter explosions that would happen if doing it for real)
        // without actually writing back the file.
        if dry_run {
            config.terminal().status(
                "Would update",
                format!(
                    "{path} from {from} to {to}",
                    path = manifest_path.display(),
                    from = bindings_dep.req,
                    to = env!("CARGO_PKG_VERSION")
                ),
            )?;
            continue;
        }

        fs::write(manifest_path, doc.to_string()).with_context(|| {
            format!(
                "failed to write manifest file `{path}`",
                path = manifest_path.display()
            )
        })?;

        config.terminal().status(
            "Updated",
            format!(
                "{path} from {from} to {to}",
                path = manifest_path.display(),
                from = bindings_dep.req,
                to = env!("CARGO_PKG_VERSION")
            ),
        )?;
    }

    Ok(())
}
