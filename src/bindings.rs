//! Module for bindings generation.

use crate::{
    last_modified_time,
    metadata::{ComponentMetadata, Ownership, Target},
    registry::PackageDependencyResolution,
};
use anyhow::{bail, Context, Result};
use cargo_component_core::registry::DecodedDependency;
use heck::ToUpperCamelCase;
use indexmap::{IndexMap, IndexSet};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::SystemTime,
};
use warg_protocol::registry::PackageId;
use wit_bindgen_core::Files;
use wit_bindgen_rust::{ExportKey, Opts};
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

/// A generator for bindings.
///
/// This type is responsible for generating the bindings
/// that the `generate!` macro will ultimately include
/// in user component projects.
pub struct BindingsGenerator<'a> {
    resolution: &'a PackageDependencyResolution<'a>,
    resolve: Resolve,
    world: WorldId,
    source_files: Vec<PathBuf>,
}

impl<'a> BindingsGenerator<'a> {
    /// Creates a new bindings generator for the given bindings directory
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
    pub fn reason(
        &self,
        last_modified_exe: SystemTime,
        last_modified_output: SystemTime,
    ) -> Result<Option<&'static str>> {
        let metadata = self.metadata();
        let exe_modified = last_modified_exe > last_modified_output;
        let manifest_modified = metadata.modified_at > last_modified_output;
        let target_modified = if let Some(path) = metadata.target_path() {
            last_modified_time(&path)? > last_modified_output
        } else {
            false
        };

        if exe_modified
            || manifest_modified
            || target_modified
            || self.dependencies_are_newer(last_modified_output)?
        {
            Ok(Some(if manifest_modified {
                "the manifest was modified"
            } else if target_modified {
                "the target WIT file was modified"
            } else if exe_modified {
                "the cargo-component executable was modified"
            } else {
                "a dependency was modified"
            }))
        } else {
            Ok(None)
        }
    }

    /// Generates the bindings source for a package.
    pub fn generate(self) -> Result<String> {
        let settings = &self.resolution.metadata.section.bindings;

        fn implementor_path_str(path: &str) -> String {
            format!("super::{path}")
        }

        fn resource_implementor(
            key: &str,
            name: &str,
            resources: &HashMap<String, String>,
        ) -> String {
            implementor_path_str(
                &resources
                    .get(key)
                    .map(Cow::Borrowed)
                    .unwrap_or_else(|| Cow::Owned(name.to_upper_camel_case())),
            )
        }

        let implementor =
            implementor_path_str(settings.implementor.as_deref().unwrap_or("Component"));

        let world = &self.resolve.worlds[self.world];
        let mut exports = HashMap::new();
        exports.insert(ExportKey::World, implementor.clone());

        for (name, item) in &world.exports {
            let key = match name {
                WorldKey::Name(name) => name.clone(),
                WorldKey::Interface(id) => {
                    let interface = &self.resolve.interfaces[*id];
                    let package = &self.resolve.packages
                        [interface.package.expect("interface must have a package")];

                    let mut key = String::new();
                    key.push_str(&package.name.namespace);
                    key.push(':');
                    key.push_str(&package.name.name);
                    key.push('/');
                    key.push_str(interface.name.as_ref().expect("interface must have a name"));
                    // wit-bindgen expects to not have the package version number in
                    // the export map, so don't append it here
                    key
                }
            };

            let implementor = match item {
                WorldItem::Interface(id) => {
                    let interface = &self.resolve.interfaces[*id];
                    for (name, ty) in &interface.types {
                        match self.resolve.types[*ty].kind {
                            TypeDefKind::Resource => {
                                let key = format!("{key}/{name}");
                                let implementor =
                                    resource_implementor(&key, name, &settings.resources);
                                exports.insert(ExportKey::Name(key), implementor);
                            }
                            _ => continue,
                        }
                    }

                    implementor.clone()
                }
                WorldItem::Type(id) => match self.resolve.types[*id].kind {
                    TypeDefKind::Resource => resource_implementor(&key, &key, &settings.resources),
                    _ => continue,
                },
                WorldItem::Function(_) => implementor.clone(),
            };

            exports.insert(ExportKey::Name(key), implementor);
        }

        let opts = Opts {
            exports,
            ownership: match settings.ownership {
                Ownership::Owning => wit_bindgen_rust::Ownership::Owning,
                Ownership::Borrowing => wit_bindgen_rust::Ownership::Borrowing {
                    duplicate_if_necessary: false,
                },
                Ownership::BorrowingDuplicateIfNecessary => {
                    wit_bindgen_rust::Ownership::Borrowing {
                        duplicate_if_necessary: true,
                    }
                }
            },
            runtime_path: Some("::cargo_component_bindings::rt".to_string()),
            bitflags_path: Some("::cargo_component_bindings::bitflags".to_string()),
            additional_derive_attributes: settings.derives.clone(),
            ..Default::default()
        };

        let mut files = Files::default();
        opts.build()
            .generate(&self.resolve, self.world, &mut files)
            .context("failed to generate bindings")?;

        let sources: Vec<_> = files
            .iter()
            .map(|(_, s)| std::str::from_utf8(s).expect("expected utf-8 bindings source"))
            .collect();
        assert!(
            sources.len() == 1,
            "expected exactly one source file to be generated"
        );

        Ok(sources[0].to_string())
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
        let (mut merged, world_id, source_files) =
            if let Target::Package { id, world, .. } = &resolution.metadata.section.target {
                Self::target_package(resolution, id, world.as_deref())?
            } else if let Some(path) = resolution.metadata.target_path() {
                Self::target_local_path(
                    resolution,
                    &path,
                    resolution.metadata.section.target.world(),
                )?
            } else {
                let (merged, world) = Self::target_empty_world(resolution);
                (merged, world, Vec::new())
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

            let pkg = &mut resolve.packages[world.package.unwrap()];
            pkg.name.namespace = id.namespace().to_string();
            pkg.name.name = id.name().to_string();

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
        let mut functions = IndexMap::default();
        let mut interfaces = IndexMap::new();
        let name;
        let docs;
        let source_pkg;

        {
            let source = &resolve.worlds[source];
            name = source.name.clone();
            docs = source.docs.clone();
            source_pkg = source.package;

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
                    _ => continue,
                }
            }
        }

        for (key, id) in interfaces {
            let named = matches!(key, WorldKey::Name(_));
            if resolve.worlds[target]
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

        // If the world had functions, insert an interface that contains them.
        if !functions.is_empty() {
            let interface = resolve.interfaces.alloc(Interface {
                name: Some(name.clone()),
                docs,
                types: Default::default(),
                functions,
                package: source_pkg,
            });

            // Add any types owned by the world to the interface
            for (id, ty) in resolve.types.iter() {
                if ty.owner == TypeOwner::World(source) {
                    resolve.interfaces[interface]
                        .types
                        .insert(ty.name.clone().expect("type should have a name"), id);
                }
            }

            // Finally, insert the interface into the target world
            if resolve.worlds[target]
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
