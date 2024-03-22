use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use clap::Args;

use cargo_component_core::command::CommonOptions;
use futures::TryStreamExt;
use semver::Version;
use tokio_util::io::{StreamReader, SyncIoBridge};
use warg_loader::{ClientConfig, PackageRef};
use wit_parser::UnresolvedPackage;

/// Pull WIT package(s) to a local "deps" directory.
#[derive(Args)]
#[clap(disable_version_flag = true)]
pub struct PullCommand {
    /// The common command options.
    #[clap(flatten)]
    pub common: CommonOptions,

    /// Use the specified registry name when pulling the package(s).
    #[clap(long, value_name = "REGISTRY")]
    pub registry: Option<String>,

    /// Update the specified directory WIT "root" directory. Dependencies will
    /// be written to e.g. "<wit-dir>/deps/<namespace>.<package>.wit".
    #[clap(long, value_name = "WIT_DIR", default_value = "wit")]
    pub wit_dir: PathBuf,

    /// Create "<wit-dir>" and "<wit-dir>/deps" directories as needed.
    #[clap(long)]
    pub create_dirs: bool,

    /// Pull the packages specified. If empty, the list of packages to pull will
    /// be parsed from "<wit-dir>/deps/*.wit".
    #[clap(value_name = "PACKAGE")]
    pub packages: Vec<PackageRef>,
}

impl PullCommand {
    /// Executes the command.
    pub async fn exec(self) -> Result<()> {
        log::debug!("executing pull command");

        let terminal = self.common.new_terminal();

        let deps_dir = self.wit_dir.join("deps");

        let deps = if self.packages.is_empty() {
            let deps = if deps_dir.exists() {
                Dep::parse_deps_files(&deps_dir)
                    .with_context(|| format!("Couldn't read existing deps from {deps_dir:?}"))?
            } else {
                vec![]
            };
            if deps.is_empty() {
                terminal.warn(format!(
                    "No deps found at {glob:?}; nothing to pull.",
                    glob = deps_dir.join("*.wit")
                ))?;
                return Ok(());
            }
            deps
        } else {
            self.packages
                .into_iter()
                .map(|pkg| Dep::specific(pkg, &deps_dir))
                .collect::<Result<Vec<_>>>()?
        };

        let mut client = {
            let mut config = ClientConfig::default();
            config.namespace_registry("wasi", "bytecodealliance.org");
            if let Some(file_config) = ClientConfig::from_default_file()? {
                config.merge_config(file_config);
            }
            config.to_client()
        };

        if !deps_dir.exists() {
            if self.create_dirs {
                log::info!("Creating {deps_dir:?}");
                std::fs::create_dir_all(&deps_dir)?;
            } else {
                bail!("Deps dir does not exist at {deps_dir:?}");
            }
        }

        for dep in deps {
            terminal.status("Pulling", format!("package {}", dep.pkg))?;
            let pkg = &dep.pkg;
            match dep.pull(&mut client).await {
                Ok(Some(version)) => {
                    terminal.status("Updated", format!("package {pkg} to version {version}"))?
                }
                Ok(None) => {
                    terminal.status("Checked", format!("package {pkg}; no updates found"))?
                }
                Err(err) => terminal.error(format!("Couldn't pull package {pkg}: {err:?}"))?,
            }
        }

        Ok(())
    }
}

struct Dep {
    path: PathBuf,
    pkg: PackageRef,
}

impl Dep {
    fn parse_file(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let pkg_name = UnresolvedPackage::parse_file(&path)?.name;
        let pkg = format!("{}:{}", pkg_name.namespace, pkg_name.name).parse()?;
        Ok(Self { path, pkg })
    }

    fn specific(pkg: PackageRef, deps_dir: &Path) -> Result<Self> {
        let path = deps_dir.join(format!("{}_{}.wit", pkg.namespace(), pkg.name()));
        if path.exists() {
            Self::parse_file(path)
        } else {
            Ok(Self { path, pkg })
        }
    }

    fn parse_deps_files(deps_dir: &Path) -> Result<Vec<Self>> {
        let mut deps = vec![];
        for entry in deps_dir
            .read_dir()
            .with_context(|| format!("couldn't read {deps_dir:?}"))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().unwrap_or_default() == "wit" && entry.file_type()?.is_file() {
                match Self::parse_file(&path) {
                    Ok(dep) => deps.push(dep),
                    Err(err) => {
                        log::warn!("Couldn't parse {path:?}: {err}");
                        log::debug!("Full error: {err:?}");
                    }
                }
            }
        }
        Ok(deps)
    }

    async fn pull(&self, client: &mut warg_loader::Client) -> Result<Option<Version>> {
        let versions = client.list_all_versions(&self.pkg).await?;
        let latest = versions.into_iter().max().context("no versions found")?;
        let release = client.get_release(&self.pkg, &latest).await?;

        let stream = client.stream_content(&self.pkg, &release).await?;
        let stream = StreamReader::new(stream.map_err(|err| match err {
            warg_loader::Error::IoError(err) => err,
            other => std::io::Error::other(other),
        }));
        let reader = SyncIoBridge::new(stream);

        let decoded = tokio::task::block_in_place(|| wit_component::decode_reader(reader))?;

        let wit =
            wit_component::WitPrinter::default().print(decoded.resolve(), decoded.package())?;

        // TODO: instead, hash existing file and compare w/ release.content_digest above
        let existing = std::fs::read(&self.path).unwrap_or_default();
        if wit.as_bytes() == existing {
            return Ok(None);
        }

        atomic_write(&self.path, wit.as_bytes())?;
        Ok(Some(latest))
    }
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file =
        tempfile::NamedTempFile::with_prefix_in(".tmp-wit-pull-", path.parent().unwrap())?;
    file.write_all(contents)?;
    file.persist(path)?;
    Ok(())
}
