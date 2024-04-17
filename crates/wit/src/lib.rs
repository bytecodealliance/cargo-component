//! The library for the WIT CLI tool.

#![deny(missing_docs)]

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use cargo_component_core::{
    lock::{LockFile, LockFileResolver, LockedPackage, LockedPackageVersion},
    registry::{
        create_client, DecodedDependency, DependencyResolutionMap, DependencyResolver,
        WargClientError, WargError,
    },
    terminal::{Colors, Terminal},
};
use config::Config;
use indexmap::{IndexMap, IndexSet};
use lock::{acquire_lock_file_ro, acquire_lock_file_rw, to_lock_file};
use std::{collections::HashSet, path::Path, time::Duration};
use warg_client::{
    storage::{ContentStorage, PublishEntry, PublishInfo},
    Retry,
};
use warg_crypto::signing::PrivateKey;
use warg_protocol::registry;
use wasm_metadata::{Link, LinkType, RegistryMetadata};
use wit_component::DecodedWasm;
use wit_parser::{PackageId, PackageName, Resolve, UnresolvedPackage};
pub mod commands;
pub mod config;
mod lock;

async fn resolve_dependencies(
    config: &Config,
    config_path: &Path,
    warg_config: &warg_client::Config,
    terminal: &Terminal,
    update_lock_file: bool,
    retry: Option<&Retry>,
) -> Result<DependencyResolutionMap, WargError> {
    let file_lock = acquire_lock_file_ro(terminal, config_path)?;
    let lock_file = file_lock
        .as_ref()
        .map(|f| {
            LockFile::read(f.file()).with_context(|| {
                format!(
                    "failed to read lock file `{path}`",
                    path = f.path().display()
                )
            })
        })
        .transpose()?;

    let mut resolver = DependencyResolver::new(
        warg_config,
        lock_file.as_ref().map(LockFileResolver::new),
        terminal,
        true,
    )?;

    for (name, dep) in &config.dependencies {
        resolver.add_dependency(name, dep, retry).await?;
    }

    let map = resolver.resolve().await?;

    // Update the lock file
    if update_lock_file {
        let new_lock_file = to_lock_file(&map);
        if Some(&new_lock_file) != lock_file.as_ref() {
            drop(file_lock);
            let file_lock = acquire_lock_file_rw(terminal, config_path)?;
            new_lock_file
                .write(file_lock.file(), "wit")
                .with_context(|| {
                    format!(
                        "failed to write lock file `{path}`",
                        path = file_lock.path().display()
                    )
                })?;
        }
    }

    Ok(map)
}

fn parse_wit_package(
    dir: &Path,
    dependencies: &DependencyResolutionMap,
) -> Result<(Resolve, PackageId), WargError> {
    let mut merged = Resolve::default();

    // Start by decoding all of the dependencies
    let mut deps = IndexMap::new();
    for (name, resolution) in dependencies {
        let decoded = resolution.decode()?;
        if let Some(prev) = deps.insert(decoded.package_name().clone(), decoded) {
            return Err(anyhow!(
            "duplicate definitions of package `{prev}` found while decoding dependency `{name}`",
            prev = prev.package_name()
        )
            .into());
        }
    }

    // Parse the root package itself
    let root = UnresolvedPackage::parse_dir(dir).with_context(|| {
        format!(
            "failed to parse package from directory `{dir}`",
            dir = dir.display()
        )
    })?;

    let mut source_files: Vec<_> = root.source_files().map(Path::to_path_buf).collect();

    // Do a topological sort of the dependencies
    let mut order = IndexSet::new();
    let mut visiting = HashSet::new();
    for dep in deps.values() {
        visit(dep, &deps, &mut order, &mut visiting)?;
    }

    assert!(visiting.is_empty());

    // Merge all of the dependencies first
    for name in order {
        match deps.swap_remove(&name).unwrap() {
            DecodedDependency::Wit {
                resolution,
                package,
            } => {
                source_files.extend(package.source_files().map(Path::to_path_buf));
                merged.push(package).with_context(|| {
                    format!(
                        "failed to merge dependency `{name}`",
                        name = resolution.name()
                    )
                })?;
            }
            DecodedDependency::Wasm {
                resolution,
                decoded,
            } => {
                let resolve = match decoded {
                    DecodedWasm::WitPackage(resolve, _) => resolve,
                    DecodedWasm::Component(resolve, _) => resolve,
                };

                merged.merge(resolve).with_context(|| {
                    format!(
                        "failed to merge world of dependency `{name}`",
                        name = resolution.name()
                    )
                })?;
            }
        };
    }

    let package = merged.push(root)?;

    return Ok((merged, package));

    fn visit<'a>(
        dep: &'a DecodedDependency<'a>,
        deps: &'a IndexMap<PackageName, DecodedDependency>,
        order: &mut IndexSet<PackageName>,
        visiting: &mut HashSet<&'a PackageName>,
    ) -> Result<(), WargError> {
        if order.contains(dep.package_name()) {
            return Ok(());
        }

        // Visit any unresolved foreign dependencies
        match dep {
            DecodedDependency::Wit {
                package,
                resolution,
            } => {
                for name in package.foreign_deps.keys() {
                    // Only visit known dependencies
                    // wit-parser will error on unknown foreign dependencies when
                    // the package is resolved
                    if let Some(dep) = deps.get(name) {
                        if !visiting.insert(name) {
                            return Err(anyhow!(
                              "foreign dependency `{name}` forms a dependency cycle while parsing dependency `{other}`", other = resolution.name()
                            )
                            .into());
                        }

                        visit(dep, deps, order, visiting)?;
                        assert!(visiting.remove(name));
                    }
                }
            }
            DecodedDependency::Wasm {
                decoded,
                resolution,
            } => {
                // Look for foreign packages in the decoded dependency
                for (_, package) in &decoded.resolve().packages {
                    if package.name.namespace == dep.package_name().namespace
                        && package.name.name == dep.package_name().name
                    {
                        continue;
                    }

                    if let Some(dep) = deps.get(&package.name) {
                        if !visiting.insert(&package.name) {
                            return Err(anyhow!(
                              "foreign dependency `{name}` forms a dependency cycle while parsing dependency `{other}`", name = package.name, other = resolution.name()
                            ).into());
                        }

                        visit(dep, deps, order, visiting)?;
                        assert!(visiting.remove(&package.name));
                    }
                }
            }
        }

        assert!(order.insert(dep.package_name().clone()));

        Ok(())
    }
}

/// Builds a WIT package given the configuration and directory to parse.
async fn build_wit_package(
    config: &Config,
    config_path: &Path,
    warg_config: &warg_client::Config,
    terminal: &Terminal,
    retry: Option<&Retry>,
) -> Result<(registry::PackageName, Vec<u8>), WargError> {
    let dependencies =
        resolve_dependencies(config, config_path, warg_config, terminal, true, retry).await?;
    let dir = config_path.parent().unwrap_or_else(|| Path::new("."));

    let (mut resolve, package) = parse_wit_package(dir, &dependencies)?;

    let pkg = &mut resolve.packages[package];
    let name = format!("{ns}:{name}", ns = pkg.name.namespace, name = pkg.name.name).parse()?;

    let bytes = wit_component::encode(Some(true), &resolve, package)?;

    let mut producers = wasm_metadata::Producers::empty();
    producers.add(
        "processed-by",
        env!("CARGO_PKG_NAME"),
        option_env!("WIT_VERSION_INFO").unwrap_or(env!("CARGO_PKG_VERSION")),
    );

    let bytes = producers
        .add_to_wasm(&bytes)
        .context("failed to add producers metadata to output WIT package")?;

    Ok((name, bytes))
}

struct PublishOptions<'a> {
    config: &'a Config,
    config_path: &'a Path,
    warg_config: &'a warg_client::Config,
    signing_key: &'a PrivateKey,
    package: Option<&'a registry::PackageName>,
    init: bool,
    dry_run: bool,
}

fn add_registry_metadata(config: &Config, bytes: &[u8]) -> Result<Vec<u8>> {
    let mut metadata = RegistryMetadata::default();
    if !config.authors.is_empty() {
        metadata.set_authors(Some(config.authors.clone()));
    }

    if !config.categories.is_empty() {
        metadata.set_categories(Some(config.categories.clone()));
    }

    metadata.set_description(config.description.clone());

    // TODO: registry metadata should have keywords
    // if !package.keywords.is_empty() {
    //     metadata.set_keywords(Some(package.keywords.clone()));
    // }

    metadata.set_license(config.license.clone());

    let mut links = Vec::new();
    if let Some(docs) = &config.documentation {
        links.push(Link {
            ty: LinkType::Documentation,
            value: docs.clone(),
        });
    }

    if let Some(homepage) = &config.homepage {
        links.push(Link {
            ty: LinkType::Homepage,
            value: homepage.clone(),
        });
    }

    if let Some(repo) = &config.repository {
        links.push(Link {
            ty: LinkType::Repository,
            value: repo.clone(),
        });
    }

    if !links.is_empty() {
        metadata.set_links(Some(links));
    }

    metadata
        .add_to_wasm(bytes)
        .context("failed to add registry metadata to component")
}

async fn publish_wit_package(
    options: PublishOptions<'_>,
    terminal: &Terminal,
    retry: Option<Retry>,
) -> Result<(), WargError> {
    let (name, bytes) = build_wit_package(
        options.config,
        options.config_path,
        options.warg_config,
        terminal,
        retry.as_ref(),
    )
    .await?;

    if options.dry_run {
        terminal.warn("not publishing package to the registry due to the --dry-run option")?;
        return Ok(());
    }

    let bytes = add_registry_metadata(options.config, &bytes)?;
    let name = options.package.unwrap_or(&name);
    let client = create_client(options.warg_config, terminal, retry.as_ref()).await?;

    let content = client
        .content()
        .store_content(
            Box::pin(futures::stream::once(async { Ok(Bytes::from(bytes)) })),
            None,
        )
        .await?;

    terminal.status("Publishing", format!("package `{name}` ({content})"))?;

    let mut info = PublishInfo {
        name: name.clone(),
        head: None,
        entries: Default::default(),
    };

    if options.init {
        info.entries.push(PublishEntry::Init);
    }

    info.entries.push(PublishEntry::Release {
        version: options.config.version.clone(),
        content,
    });

    let record_id = client
        .publish_with_info(options.signing_key, info)
        .await
        .map_err(|e| WargClientError(e))?;

    client
        .wait_for_publish(name, &record_id, Duration::from_secs(1))
        .await
        .map_err(|e| WargClientError(e))?;

    terminal.status(
        "Published",
        format!(
            "package `{name}` v{version}",
            version = options.config.version
        ),
    )?;

    Ok(())
}

/// Update the dependencies in the lock file.
pub async fn update_lockfile(
    config: &Config,
    config_path: &Path,
    warg_config: &warg_client::Config,
    terminal: &Terminal,
    dry_run: bool,
    retry: Option<Retry>,
) -> Result<()> {
    // Resolve all dependencies as if the lock file does not exist
    let mut resolver = DependencyResolver::new(warg_config, None, terminal, true)?;
    for (name, dep) in &config.dependencies {
        resolver.add_dependency(name, dep, retry.as_ref()).await?;
    }

    let map = resolver.resolve().await?;

    let file_lock = acquire_lock_file_ro(terminal, config_path)?;
    let orig_lock_file = file_lock
        .as_ref()
        .map(|f| {
            LockFile::read(f.file()).with_context(|| {
                format!(
                    "failed to read lock file `{path}`",
                    path = f.path().display()
                )
            })
        })
        .transpose()?
        .unwrap_or_default();

    let new_lock_file = to_lock_file(&map);

    for old_pkg in &orig_lock_file.packages {
        let new_pkg = match new_lock_file
            .packages
            .binary_search_by_key(&old_pkg.key(), LockedPackage::key)
            .map(|index| &new_lock_file.packages[index])
        {
            Ok(pkg) => pkg,
            Err(_) => {
                // The package is no longer a dependency
                for old_ver in &old_pkg.versions {
                    terminal.status_with_color(
                        if dry_run { "Would remove" } else { "Removing" },
                        format!(
                            "dependency `{name}` v{version}",
                            name = old_pkg.name,
                            version = old_ver.version,
                        ),
                        Colors::Red,
                    )?;
                }
                continue;
            }
        };

        for old_ver in &old_pkg.versions {
            let new_ver = match new_pkg
                .versions
                .binary_search_by_key(&old_ver.key(), LockedPackageVersion::key)
                .map(|index| &new_pkg.versions[index])
            {
                Ok(ver) => ver,
                Err(_) => {
                    // The version of the package is no longer a dependency
                    terminal.status_with_color(
                        if dry_run { "Would remove" } else { "Removing" },
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

            // The version has changed
            if old_ver.version != new_ver.version {
                terminal.status_with_color(
                    if dry_run { "Would update" } else { "Updating" },
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
    }

    for new_pkg in &new_lock_file.packages {
        let old_pkg = match orig_lock_file
            .packages
            .binary_search_by_key(&new_pkg.key(), LockedPackage::key)
            .map(|index| &orig_lock_file.packages[index])
        {
            Ok(pkg) => pkg,
            Err(_) => {
                // The package is new
                for new_ver in &new_pkg.versions {
                    terminal.status_with_color(
                        if dry_run { "Would add" } else { "Adding" },
                        format!(
                            "dependency `{name}` v{version}",
                            name = new_pkg.name,
                            version = new_ver.version,
                        ),
                        Colors::Green,
                    )?;
                }
                continue;
            }
        };

        for new_ver in &new_pkg.versions {
            if old_pkg
                .versions
                .binary_search_by_key(&new_ver.key(), LockedPackageVersion::key)
                .map(|index| &old_pkg.versions[index])
                .is_err()
            {
                // The version is new
                terminal.status_with_color(
                    if dry_run { "Would add" } else { "Adding" },
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

    if dry_run {
        terminal.warn("not updating lock file due to --dry-run option")?;
    } else {
        // Update the lock file
        if new_lock_file != orig_lock_file {
            drop(file_lock);
            let file_lock = acquire_lock_file_rw(terminal, config_path)?;
            new_lock_file
                .write(file_lock.file(), "wit")
                .with_context(|| {
                    format!(
                        "failed to write lock file `{path}`",
                        path = file_lock.path().display()
                    )
                })?;
        }
    }

    Ok(())
}
