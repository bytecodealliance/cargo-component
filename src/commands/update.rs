use anyhow::Result;
use cargo_component_core::{command::CommonOptions, terminal::Colors};
use clap::Args;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use terminal_link::Link as TerminalLink;
use wasm_pkg_core::{
    lock::{LockFile, LockedPackageVersion},
    resolver::{DependencyResolver, RegistryPackage},
};

use crate::{
    load_component_metadata, load_metadata, metadata::ComponentMetadata, Config,
    PackageComponentMetadata,
};

/// Update dependencies as recorded in the component lock file
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct UpdateCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Don't actually write the lockfile
    #[clap(long = "dry-run")]
    pub dry_run: bool,

    /// Require lock file and cache are up to date
    #[clap(long = "frozen")]
    pub frozen: bool,

    /// Path to Cargo.toml
    #[clap(long = "manifest-path", value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Require lock file is up to date
    #[clap(long = "locked")]
    pub locked: bool,

    /// Run without accessing the network
    #[clap(long = "offline")]
    pub offline: bool,
}

impl UpdateCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing update command");
        let config = Config::new(self.common.new_terminal(), self.common.config).await?;
        let metadata = load_metadata(self.manifest_path.as_deref())?;
        let packages = load_component_metadata(&metadata, [].iter(), true)?;
        let client = config.client(self.common.cache_dir, false).await?;
        let lock_file = if Path::exists(&PathBuf::from("Cargo-component.lock")) {
            config.terminal().status_with_color(
                "Warning",
                format!(
                    "It seems you are using `Cargo-component.lock` for your lock file.
               As of version 0.20.0, cargo-component uses `wkg.lock` from {}.
               It is recommended you switch to `wkg.lock` by deleting your `Cargo-component.lock",
                    TerminalLink::new(
                        "wasm-pkg-tools",
                        "https://github.com/bytecodealliance/wasm-pkg-tools"
                    )
                ),
                Colors::Yellow,
            )?;
            LockFile::load_from_path("Cargo-component.lock", true).await?
        } else {
            LockFile::load(true).await?
        };
        let old_pkgs = lock_file.packages.clone();
        drop(lock_file);

        let mut new_packages = HashSet::new();
        for PackageComponentMetadata {
            metadata: ComponentMetadata { section, .. },
            ..
        } in &packages
        {
            let target_deps = section.target.dependencies();
            for (name, dep) in target_deps.iter() {
                match &dep.0 {
                    wasm_pkg_core::resolver::Dependency::Package(RegistryPackage {
                        version,
                        ..
                    }) => {
                        new_packages.insert((name.clone(), version.clone()));
                    }
                    wasm_pkg_core::resolver::Dependency::Local(_) => todo!(),
                }
            }
            for (name, dep) in section.dependencies.iter() {
                match &dep.0 {
                    wasm_pkg_core::resolver::Dependency::Package(RegistryPackage {
                        version,
                        ..
                    }) => {
                        new_packages.insert((name.clone(), version.clone()));
                    }
                    wasm_pkg_core::resolver::Dependency::Local(_) => todo!(),
                }
            }
        }
        let mut resolver = DependencyResolver::new_with_client(client, None)?;
        resolver.add_packages(new_packages).await?;
        let deps = resolver.resolve().await?;

        let mut new_lock_file = LockFile::from_dependencies(&deps, "wkg.lock").await?;

        for old_pkg in &old_pkgs {
            if let Some(new_pkg) = new_lock_file
                .packages
                .iter()
                .find(|p| p.name == old_pkg.name)
            {
                for old_ver in &old_pkg.versions {
                    let new_ver = match new_pkg
                        .versions
                        .binary_search_by_key(&old_ver.key(), LockedPackageVersion::key)
                        .map(|index| &new_pkg.versions[index])
                    {
                        Ok(ver) => ver,
                        Err(_) => {
                            config.terminal().status_with_color(
                                if self.dry_run {
                                    "Would remove"
                                } else {
                                    "Removing"
                                },
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
                    if old_ver.version != new_ver.version {
                        config.terminal().status_with_color(
                            if self.dry_run {
                                "Would update"
                            } else {
                                "Updating"
                            },
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
            } else {
                for old_ver in &old_pkg.versions {
                    config.terminal().status_with_color(
                        if self.dry_run {
                            "Would remove"
                        } else {
                            "Removing"
                        },
                        format!("dependency `{}` v{}", old_pkg.name, old_ver.version),
                        Colors::Red,
                    )?;
                }
            }
        }
        for new_pkg in &new_lock_file.packages {
            if old_pkgs.iter().find(|p| p.name == new_pkg.name).is_none() {
                for new_ver in &new_pkg.versions {
                    config.terminal().status_with_color(
                        if self.dry_run { "Would add" } else { "Adding" },
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

        new_lock_file.write().await?;
        Ok(())
    }
}
