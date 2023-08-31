//! The library for the WIT CLI tool.

#![deny(missing_docs)]

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use cargo_component_core::{
    lock::{LockFile, LockFileResolver, LockedPackage, LockedPackageVersion},
    registry::{create_client, DecodedDependency, DependencyResolutionMap, DependencyResolver},
    terminal::{Colors, Terminal},
};
use config::Config;
use indexmap::{IndexMap, IndexSet};
use lock::{acquire_lock_file_ro, acquire_lock_file_rw, to_lock_file};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};
use warg_client::storage::{ContentStorage, PublishEntry, PublishInfo};
use warg_crypto::signing::PrivateKey;
use warg_protocol::registry::PackageId;
use wit_component::DecodedWasm;
use wit_parser::{PackageName, Resolve, UnresolvedPackage};

pub mod commands;
pub mod config;
mod lock;

async fn resolve_dependencies(
    config: &Config,
    config_path: &Path,
    warg_config: &warg_client::Config,
    terminal: &Terminal,
    update_lock_file: bool,
) -> Result<DependencyResolutionMap> {
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
        &config.registries,
        lock_file.as_ref().map(LockFileResolver::new),
        terminal,
        true,
    )?;

    for (id, dep) in &config.dependencies {
        resolver.add_dependency(id, dep).await?;
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
) -> Result<(Resolve, wit_parser::PackageId)> {
    let mut merged = Resolve::default();

    // Start by decoding all of the dependencies
    let mut deps = IndexMap::new();
    let mut unversioned: HashMap<_, Vec<_>> = HashMap::new();
    for (id, resolution) in dependencies {
        let decoded = resolution.decode()?;
        let name = decoded.package_name();

        let versionless = PackageName {
            namespace: name.namespace.clone(),
            name: name.name.clone(),
            version: None,
        };

        let (index, prev) = deps.insert_full(name.clone(), decoded);
        if let Some(prev) = prev {
            bail!(
                "duplicate definitions of package `{name}` found while decoding dependency `{id}`",
                name = prev.package_name()
            );
        }

        // We're storing the dependencies with versionless package ids
        // This allows us to resolve a versionless foreign dependency to a singular
        // versioned dependency, if there is one
        unversioned.entry(versionless).or_default().push(index);
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
        visit(dep, &deps, &unversioned, &mut order, &mut visiting)?;
    }

    assert!(visiting.is_empty());

    // Merge all of the dependencies first
    for name in order {
        match deps.remove(&name).unwrap() {
            DecodedDependency::Wit {
                resolution,
                package,
            } => {
                source_files.extend(package.source_files().map(Path::to_path_buf));
                merged.push(package).with_context(|| {
                    format!("failed to merge dependency `{id}`", id = resolution.id())
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
                        "failed to merge world of dependency `{id}`",
                        id = resolution.id()
                    )
                })?;
            }
        };
    }

    let package = merged.push(root).with_context(|| {
        format!(
            "failed to merge package from directory `{dir}`",
            dir = dir.display()
        )
    })?;

    return Ok((merged, package));

    fn visit<'a>(
        dep: &'a DecodedDependency<'a>,
        deps: &'a IndexMap<PackageName, DecodedDependency>,
        unversioned: &HashMap<PackageName, Vec<usize>>,
        order: &mut IndexSet<PackageName>,
        visiting: &mut HashSet<&'a PackageName>,
    ) -> Result<()> {
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
                    if !visiting.insert(name) {
                        bail!("foreign dependency `{name}` forms a dependency cycle while parsing dependency `{id}`", id = resolution.id());
                    }

                    // Only visit known dependencies
                    // wit-parser will error on unknown foreign dependencies when
                    // the package is resolved
                    match deps.get(name) {
                        Some(dep) => {
                            // Exact match on the dependency; visit it
                            visit(dep, deps, unversioned, order, visiting)?
                        }
                        None => match unversioned.get(name) {
                            // Only visit if there's exactly one unversioned dependency
                            // If there's more than one, it's ambiguous and wit-parser
                            // will error when the package is resolved.
                            Some(indexes) if indexes.len() == 1 => {
                                let dep = &deps[indexes[0]];
                                visit(dep, deps, unversioned, order, visiting)?;
                            }
                            _ => {}
                        },
                    }

                    assert!(visiting.remove(name));
                }
            }
            DecodedDependency::Wasm { .. } => {
                // No unresolved foreign dependencies for decoded wasm files
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
) -> Result<(PackageId, Vec<u8>)> {
    let dependencies =
        resolve_dependencies(config, config_path, warg_config, terminal, true).await?;

    let dir = config_path.parent().unwrap_or_else(|| Path::new("."));

    let (mut resolve, package) = parse_wit_package(dir, &dependencies)?;

    let pkg = &mut resolve.packages[package];
    let id = format!("{ns}:{name}", ns = pkg.name.namespace, name = pkg.name.name).parse()?;

    let bytes = wit_component::encode(&resolve, package)?;

    let mut producers = wasm_metadata::Producers::empty();
    producers.add(
        "processed-by",
        env!("CARGO_PKG_NAME"),
        option_env!("WIT_VERSION_INFO").unwrap_or(env!("CARGO_PKG_VERSION")),
    );

    let bytes = producers
        .add_to_wasm(&bytes)
        .context("failed to add producers metadata to output WIT package")?;

    Ok((id, bytes))
}

struct PublishOptions<'a> {
    config: &'a Config,
    config_path: &'a Path,
    warg_config: &'a warg_client::Config,
    url: &'a str,
    signing_key: &'a PrivateKey,
    package: Option<&'a PackageId>,
    init: bool,
    dry_run: bool,
}

async fn publish_wit_package(options: PublishOptions<'_>, terminal: &Terminal) -> Result<()> {
    let (id, bytes) = build_wit_package(
        options.config,
        options.config_path,
        options.warg_config,
        terminal,
    )
    .await?;

    if options.dry_run {
        terminal.warn("not publishing package to the registry due to the --dry-run option")?;
        return Ok(());
    }

    let id = options.package.unwrap_or(&id);
    let client = create_client(options.warg_config, options.url, terminal)?;

    let content = client
        .content()
        .store_content(
            Box::pin(futures::stream::once(async { Ok(Bytes::from(bytes)) })),
            None,
        )
        .await?;

    terminal.status("Publishing", format!("package `{id}` ({content})",))?;

    let mut info = PublishInfo {
        id: id.clone(),
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

    let record_id = client.publish_with_info(options.signing_key, info).await?;
    client
        .wait_for_publish(id, &record_id, Duration::from_secs(1))
        .await?;

    terminal.status(
        "Published",
        format!(
            "package `{id}` v{version}",
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
) -> Result<()> {
    // Resolve all dependencies as if the lock file does not exist
    let mut resolver =
        DependencyResolver::new(warg_config, &config.registries, None, terminal, true)?;
    for (id, dep) in &config.dependencies {
        resolver.add_dependency(id, dep).await?;
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
                            "dependency `{id}` v{version}",
                            id = old_pkg.id,
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
                            "dependency `{id}` v{version}",
                            id = old_pkg.id,
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
                        "dependency `{id}` v{old} -> v{new}",
                        id = old_pkg.id,
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
                            "dependency `{id}` v{version}",
                            id = new_pkg.id,
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
                        "dependency `{id}` v{version}",
                        id = new_pkg.id,
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
