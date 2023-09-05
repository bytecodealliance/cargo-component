//! Module for bindings generation.

use crate::{
    last_modified_time,
    metadata::{ComponentMetadata, Target},
    registry::PackageDependencyResolution,
};
use anyhow::{bail, Context, Result};
use cargo_component_core::registry::DecodedDependency;
use indexmap::{IndexMap, IndexSet};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::SystemTime,
};
use warg_protocol::registry::PackageId;
use wit_component::DecodedWasm;
use wit_parser::{
    Interface, Package, PackageName, Resolve, Type, TypeDefKind, TypeOwner, UnresolvedPackage,
    World, WorldId, WorldItem, WorldKey,
};

fn named_world_key<'a>(resolve: &'a Resolve, orig: &'a WorldKey, prefix: &str) -> WorldKey {
    let name = match orig {
        WorldKey::Name(n) => n,
        WorldKey::Interface(id) => {
            let iface = &resolve.interfaces[*id];
            iface.name.as_ref().expect("unnamed interface")
        }
    };

    WorldKey::Name(format!("{prefix}-{name}"))
}

/// An encoder for bindings information.
///
/// This type is responsible for encoding the target world
/// into a binary wasm file that the `generate!` macro
/// will use for generating the bindings.
pub struct BindingsEncoder<'a> {
    resolution: &'a PackageDependencyResolution<'a>,
    resolve: Resolve,
    world: WorldId,
    source_files: Vec<PathBuf>,
}

impl<'a> BindingsEncoder<'a> {
    /// Creates a new bindings encoder for the given bindings directory
    /// and package dependency resolution.
    pub fn new(resolution: &'a PackageDependencyResolution<'a>) -> Result<Self> {
        let (resolve, world, source_files) =
            Self::create_target_world(resolution).with_context(|| {
                format!(
                    "failed to create a target world for package `{name}` ({path})",
                    name = resolution.metadata.name,
                    path = resolution.metadata.manifest_path.display()
                )
            })?;

        Ok(Self {
            resolution,
            resolve,
            world,
            source_files,
        })
    }

    /// Gets the cargo metadata for the package that the bindings are for.
    pub fn metadata(&self) -> &ComponentMetadata {
        self.resolution.metadata
    }

    /// Gets the reason for generating the bindings.
    ///
    /// If this returns `Ok(None)`, then the bindings are up-to-date and
    /// do not need to be regenerated.
    pub fn reason(&self, last_modified_output: SystemTime) -> Result<Option<&'static str>> {
        let metadata = self.metadata();
        let manifest_modified = metadata.modified_at > last_modified_output;
        let target_modified = if let Some(Target::Local { path, .. }) = &metadata.section.target {
            last_modified_time(path)? > last_modified_output
        } else {
            false
        };

        if manifest_modified
            || target_modified
            || self.dependencies_are_newer(last_modified_output)?
        {
            Ok(Some(if manifest_modified {
                "the manifest was modified"
            } else if target_modified {
                "the target WIT file was modified"
            } else {
                "a dependency was modified"
            }))
        } else {
            Ok(None)
        }
    }

    /// Encodes the target world to a binary format.
    pub fn encode(mut self) -> Result<Vec<u8>> {
        let world = &self.resolve.worlds[self.world];
        let pkg_id = world.package.context("world has no package")?;
        let pkg = &mut self.resolve.packages[pkg_id];

        self.resolve
            .package_names
            .remove(&pkg.name)
            .with_context(|| format!("package name `{name}` is not in map", name = pkg.name))?;

        if self
            .resolve
            .package_names
            .insert(pkg.name.clone(), pkg_id)
            .is_some()
        {
            bail!("duplicate package name `{name}`", name = pkg.name);
        }

        wit_component::encode(
            &self.resolve,
            self.resolve.worlds[self.world]
                .package
                .context("world has no package")?,
        )
    }

    fn dependencies_are_newer(&self, last_modified_output: SystemTime) -> Result<bool> {
        for dep in &self.source_files {
            if last_modified_time(dep)? > last_modified_output {
                log::debug!(
                    "target source file `{path}` has been modified",
                    path = dep.display()
                );
                return Ok(true);
            }
        }

        for (_, dep) in self.resolution.all() {
            if last_modified_time(dep.path())? > last_modified_output {
                log::debug!(
                    "dependency `{path}` has been modified",
                    path = dep.path().display()
                );
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn create_target_world(
        resolution: &PackageDependencyResolution,
    ) -> Result<(Resolve, WorldId, Vec<PathBuf>)> {
        let (mut merged, world_id, source_files) = match &resolution.metadata.section.target {
            Some(Target::Package { id, world, .. }) => {
                Self::target_package(resolution, id, world.as_deref())?
            }
            Some(Target::Local { path, world, .. }) => {
                Self::target_local_path(resolution, path, world.as_deref())?
            }
            None => {
                let (merged, world) = Self::target_empty_world(resolution);
                (merged, world, Vec::new())
            }
        };

        // Merge all component dependencies as interface imports
        for (id, dependency) in &resolution.resolutions {
            let (mut resolve, component_world_id) = dependency
                .decode()?
                .into_component_world()
                .with_context(|| format!("failed to decode component dependency `{id}`"))?;

            // Set the world name as currently it defaults to "root"
            // For now, set it to the name from the id
            let world = &mut resolve.worlds[component_world_id];
            world.name = id.name().to_string();

            let source = merged
                .merge(resolve)
                .with_context(|| format!("failed to merge world of dependency `{id}`"))?
                .worlds[component_world_id.index()];
            Self::import_world(&mut merged, source, world_id)?;
        }

        Ok((merged, world_id, source_files))
    }

    fn target_package(
        resolution: &PackageDependencyResolution,
        id: &PackageId,
        world: Option<&str>,
    ) -> Result<(Resolve, WorldId, Vec<PathBuf>)> {
        // We must have resolved a target package dependency at this point
        assert_eq!(resolution.target_resolutions.len(), 1);

        // Decode the target package dependency
        let dependency = resolution.target_resolutions.values().next().unwrap();
        let (resolve, pkg, source_files) = dependency.decode()?.resolve().with_context(|| {
            format!(
                "failed to resolve target package `{id}`",
                id = dependency.id()
            )
        })?;

        let world = resolve
            .select_world(pkg, world)
            .with_context(|| format!("failed to select world from target package `{id}`"))?;

        Ok((resolve, world, source_files))
    }

    fn target_local_path(
        resolution: &PackageDependencyResolution,
        path: &Path,
        world: Option<&str>,
    ) -> Result<(Resolve, WorldId, Vec<PathBuf>)> {
        let mut merged = Resolve::default();

        // Start by decoding all of the target dependencies
        let mut deps = IndexMap::new();
        for (id, resolution) in &resolution.target_resolutions {
            let decoded = resolution.decode()?;
            let name = decoded.package_name();

            if let Some(prev) = deps.insert(name.clone(), decoded) {
                bail!("duplicate definitions of package `{name}` found while decoding target dependency `{id}`", name = prev.package_name());
            }
        }

        // Parse the target package itself
        let root = if path.is_dir() {
            UnresolvedPackage::parse_dir(path).with_context(|| {
                format!(
                    "failed to parse local target from directory `{}`",
                    path.display()
                )
            })?
        } else {
            UnresolvedPackage::parse_file(path).with_context(|| {
                format!(
                    "failed to parse local target `{path}`",
                    path = path.display()
                )
            })?
        };

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
            match deps.remove(&name).unwrap() {
                DecodedDependency::Wit {
                    resolution,
                    package,
                } => {
                    source_files.extend(package.source_files().map(Path::to_path_buf));
                    merged.push(package).with_context(|| {
                        format!(
                            "failed to merge target dependency `{id}`",
                            id = resolution.id()
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
                            "failed to merge world of target dependency `{id}`",
                            id = resolution.id()
                        )
                    })?;
                }
            }
        }

        let package = merged.push(root).with_context(|| {
            format!(
                "failed to merge local target `{path}`",
                path = path.display()
            )
        })?;

        let world = merged
            .select_world(package, world)
            .with_context(|| match world {
                Some(world) => {
                    format!(
                        "failed to select the specified world `{world}` for local target `{path}`",
                        path = path.display()
                    )
                }
                None => format!(
                    "failed to select the default world to use for local target `{path}`",
                    path = path.display()
                ),
            })?;

        return Ok((merged, world, source_files));

        fn visit<'a>(
            dep: &'a DecodedDependency<'a>,
            deps: &'a IndexMap<PackageName, DecodedDependency>,
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
                            bail!("foreign dependency `{name}` forms a dependency cycle while parsing target dependency `{id}`", id = resolution.id());
                        }

                        // Only visit known dependencies
                        // wit-parser will error on unknown foreign dependencies when
                        // the package is resolved
                        if let Some(dep) = deps.get(name) {
                            visit(dep, deps, order, visiting)?
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

    fn target_empty_world(resolution: &PackageDependencyResolution) -> (Resolve, WorldId) {
        let mut resolve = Resolve::default();
        let name = resolution.metadata.name.clone();
        let pkg_name = PackageName {
            namespace: "component".to_string(),
            name: name.clone(),
            version: None,
        };

        let package = resolve.packages.alloc(Package {
            name: pkg_name.clone(),
            interfaces: Default::default(),
            worlds: Default::default(),
            docs: Default::default(),
        });

        resolve.package_names.insert(pkg_name, package);

        let world = resolve.worlds.alloc(World {
            name: name.clone(),
            docs: Default::default(),
            imports: Default::default(),
            exports: Default::default(),
            package: Some(package),
            includes: Default::default(),
            include_names: Default::default(),
        });

        resolve.packages[package].worlds.insert(name, world);

        (resolve, world)
    }

    // This function imports in the target world the exports of the source world.
    //
    // This is used for dependencies on other components so that their exports may
    // be imported by the component being built.
    fn import_world(resolve: &mut Resolve, source: WorldId, target: WorldId) -> Result<()> {
        let mut types = IndexMap::default();
        let mut functions = IndexMap::default();
        let mut interfaces = IndexMap::new();
        let name;

        {
            let source = &resolve.worlds[source];
            name = source.name.clone();

            // Check for imported types, which must also import any owning interfaces
            for item in source.imports.values() {
                if let WorldItem::Type(ty) = &item {
                    if let TypeDefKind::Type(Type::Id(ty)) = resolve.types[*ty].kind {
                        if let TypeOwner::Interface(i) = resolve.types[ty].owner {
                            interfaces.insert(WorldKey::Interface(i), i);
                        }
                    }
                }
            }

            // Add imports for all exported items
            for (key, item) in &source.exports {
                match item {
                    WorldItem::Function(f) => {
                        functions.insert(key.clone().unwrap_name(), f.clone());
                    }
                    WorldItem::Interface(i) => {
                        interfaces.insert(named_world_key(resolve, key, &name), *i);
                    }
                    WorldItem::Type(t) => {
                        types.insert(key.clone().unwrap_name(), *t);
                    }
                }
            }
        }

        let target = &mut resolve.worlds[target];
        for (key, id) in interfaces {
            let named = matches!(key, WorldKey::Name(_));
            if target
                .imports
                .insert(key, WorldItem::Interface(id))
                .is_some()
                && named
            {
                let iface = &resolve.interfaces[id];
                let pkg = &resolve.packages[iface.package.expect("interface has no package")];
                let id = pkg
                    .name
                    .interface_id(iface.name.as_deref().expect("interface has no name"));
                bail!("cannot import dependency `{id}` because it conflicts with an import in the target world");
            }
        }

        if !types.is_empty() || !functions.is_empty() {
            let interface = resolve.interfaces.alloc(Interface {
                name: Some(name.clone()),
                docs: Default::default(),
                types,
                functions,
                package: target.package,
            });

            if target
                .imports
                .insert(
                    WorldKey::Name(name.clone()),
                    WorldItem::Interface(interface),
                )
                .is_some()
            {
                bail!("cannot import dependency `{name}` because it conflicts with an import in the target world");
            }
        }

        Ok(())
    }
}
