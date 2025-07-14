//! Module for bindings generation.
use std::{
    collections::{HashMap, HashSet},
    mem,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use cargo_component_core::registry::DecodedDependency;
use heck::ToKebabCase;
use indexmap::{IndexMap, IndexSet};
use semver::Version;
use wasm_pkg_client::PackageRef;
use wit_bindgen_core::Files;
use wit_bindgen_rust::{AsyncConfig, Opts, WithOption};
use wit_component::DecodedWasm;
use wit_parser::{
    Interface, Package, PackageName, Resolve, Type, TypeDefKind, TypeOwner, UnresolvedPackageGroup,
    World, WorldId, WorldItem, WorldKey,
};

use crate::{metadata::Ownership, registry::PackageDependencyResolution};

// Used to format `unlocked-dep` import names for dependencies on
// other components.
fn format_dep_import(package: &Package, name: Option<&str>, version: Option<&Version>) -> String {
    match (name, version) {
        (Some(name), Some(version)) => format!(
            "unlocked-dep=<{ns}:{pkg}/{name}@{{>={min} <{max}}}>",
            ns = package.name.namespace,
            pkg = package.name.name,
            min = version,
            max = Version::new(version.major, version.minor + 1, 0)
        ),
        (Some(name), None) => format!(
            "unlocked-dep=<{ns}:{pkg}/{name}>",
            ns = package.name.namespace,
            pkg = package.name.name
        ),
        (None, Some(version)) => format!(
            "unlocked-dep=<{ns}:{pkg}@{{>={min} <{max}}}>",
            ns = package.name.namespace,
            pkg = package.name.name,
            min = version,
            max = Version::new(version.major, version.minor + 1, 0)
        ),
        (None, None) => format!(
            "unlocked-dep=<{ns}:{pkg}>",
            ns = package.name.namespace,
            pkg = package.name.name
        ),
    }
}

/// A generator for bindings.
///
/// This type is responsible for generating the bindings
/// in user component projects.
pub struct BindingsGenerator<'a> {
    resolution: &'a PackageDependencyResolution<'a>,
    resolve: Resolve,
    world: WorldId,
}

impl<'a> BindingsGenerator<'a> {
    /// Creates a new bindings generator for the given bindings directory
    /// and package dependency resolution.
    ///
    /// Returns a tuple of the bindings generator and a map of import names.
    pub async fn new(
        resolution: &'a PackageDependencyResolution<'a>,
    ) -> Result<Option<(Self, HashMap<String, String>)>> {
        let mut import_name_map = Default::default();
        match Self::create_target_world(resolution, &mut import_name_map)
            .await
            .with_context(|| {
                format!(
                    "failed to create a target world for package `{name}` ({path})",
                    name = resolution.metadata.name,
                    path = resolution.metadata.manifest_path.display()
                )
            })? {
            Some((resolve, world, _)) => Ok(Some((
                Self {
                    resolution,
                    resolve,
                    world,
                },
                import_name_map,
            ))),
            None => Ok(None),
        }
    }

    /// Generates the bindings source for a package.
    pub fn generate(self) -> Result<String> {
        let settings = &self.resolution.metadata.section.bindings;
        let opts = Opts {
            format: settings.format,
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
            additional_derive_attributes: settings.derives.clone(),
            additional_derive_ignore: Vec::new(),
            std_feature: settings.std_feature,
            // We use pregenerated bindings, rather than the `generate!` macro
            // from the `wit-bindgen` crate, so instead of getting the runtime
            // from the default path of `wit_bindgen::rt`, which is a re-export
            // of the `wit-bindgen-rt` API, we just use the `wit-bindgen-rt`
            // crate directly.
            runtime_path: Some("wit_bindgen_rt".to_string()),
            bitflags_path: None,
            raw_strings: settings.raw_strings,
            skip: settings.skip.clone(),
            stubs: settings.stubs,
            export_prefix: settings.export_prefix.clone(),
            with: settings
                .with
                .iter()
                .map(|(key, value)| (key.clone(), WithOption::Path(value.clone())))
                .collect(),
            generate_all: settings.generate_all,
            type_section_suffix: settings.type_section_suffix.clone(),
            disable_run_ctors_once_workaround: settings.disable_run_ctors_once_workaround,
            default_bindings_module: settings.default_bindings_module.clone(),
            export_macro_name: settings.export_macro_name.clone(),
            pub_export_macro: settings.pub_export_macro,
            generate_unused_types: settings.generate_unused_types,
            disable_custom_section_link_helpers: settings.disable_custom_section_link_helpers,

            // TODO: pipe this through to the CLI options, requires valid serde impls
            async_: AsyncConfig::None,
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

    async fn create_target_world(
        resolution: &PackageDependencyResolution<'_>,
        import_name_map: &mut HashMap<String, String>,
    ) -> Result<Option<(Resolve, WorldId, Vec<PathBuf>)>> {
        log::debug!(
            "creating target world for package `{name}` ({path})",
            name = resolution.metadata.name,
            path = resolution.metadata.manifest_path.display()
        );

        // A flag used to determine whether the target is empty. It must meet two conditions:
        // no wit files and no dependencies.
        let mut empty_target = false;
        let (mut merged, world_id, source_files) = if let Some(name) =
            resolution.metadata.target_package()
        {
            Self::target_package(resolution, name, resolution.metadata.target_world()).await?
        } else if let Some(path) = resolution.metadata.target_path() {
            Self::target_local_path(resolution, &path, resolution.metadata.target_world()).await?
        } else {
            empty_target = true;
            let (merged, world) = Self::target_empty_world(resolution);
            (merged, world, Vec::new())
        };

        // Merge all component dependencies as interface imports
        for (id, dependency) in &resolution.resolutions {
            log::debug!("importing component dependency `{id}`");
            empty_target = false;

            let (mut resolve, component_world_id) = dependency
                .decode()
                .await?
                .into_component_world()
                .with_context(|| format!("failed to decode component dependency `{id}`"))?;

            // Set the world name as currently it defaults to "root"
            // For now, set it to the name from the id
            let world = &mut resolve.worlds[component_world_id];
            let old_name = mem::replace(&mut world.name, id.name().to_string());

            let pkg = &mut resolve.packages[world.package.unwrap()];
            pkg.name.namespace = id.namespace().to_string();
            pkg.name.name = id.name().to_string();

            // Update the world name in the `pkg.worlds` map too. Don't use
            // `MutableKeys` because the new world name may not have the same
            // hash as the old world name.
            let mut new_worlds = IndexMap::new();
            for (name, world) in pkg.worlds.iter() {
                if name == &old_name {
                    new_worlds.insert(id.name().to_string(), *world);
                } else {
                    new_worlds.insert(name.clone(), *world);
                }
            }
            assert_eq!(pkg.worlds.len(), new_worlds.len());
            pkg.worlds = new_worlds;

            let source = merged
                .merge(resolve)
                .with_context(|| format!("failed to merge world of dependency `{id}`"))?
                .worlds[component_world_id.index()]
            .unwrap();
            Self::import_world(
                &mut merged,
                source,
                world_id,
                dependency.version(),
                import_name_map,
            )?;
        }

        if empty_target {
            return Ok(None);
        };
        Ok(Some((merged, world_id, source_files)))
    }

    async fn target_package(
        resolution: &PackageDependencyResolution<'_>,
        name: &PackageRef,
        world: Option<&str>,
    ) -> Result<(Resolve, WorldId, Vec<PathBuf>)> {
        // We must have resolved a target package dependency at this point
        assert_eq!(resolution.target_resolutions.len(), 1);

        // Decode the target package dependency
        let dependency = resolution.target_resolutions.values().next().unwrap();
        let (resolve, pkg, source_files) =
            dependency.decode().await?.resolve().with_context(|| {
                format!(
                    "failed to resolve target package `{name}`",
                    name = dependency.name()
                )
            })?;

        let world = resolve
            .select_world(pkg, world)
            .with_context(|| format!("failed to select world from target package `{name}`"))?;

        Ok((resolve, world, source_files))
    }

    async fn target_local_path(
        resolution: &PackageDependencyResolution<'_>,
        path: &Path,
        world: Option<&str>,
    ) -> Result<(Resolve, WorldId, Vec<PathBuf>)> {
        let mut merged = Resolve::default();

        // Start by decoding all of the target dependencies
        let mut deps = IndexMap::new();
        for (id, resolution) in &resolution.target_resolutions {
            let decoded = resolution.decode().await?;
            let name = decoded.package_name();

            if let Some(prev) = deps.insert(name.clone(), decoded) {
                bail!("duplicate definitions of package `{name}` found while decoding target dependency `{id}`", name = prev.package_name());
            }
        }

        // Parse the target package itself
        let root = if path.is_dir() {
            UnresolvedPackageGroup::parse_dir(path).with_context(|| {
                format!(
                    "failed to parse local target from directory `{}`",
                    path.display()
                )
            })?
        } else {
            UnresolvedPackageGroup::parse_file(path).with_context(|| {
                format!(
                    "failed to parse local target `{path}`",
                    path = path.display()
                )
            })?
        };

        let mut source_files: Vec<_> = root
            .source_map
            .source_files()
            .map(Path::to_path_buf)
            .collect();

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
                    source_files.extend(package.source_map.source_files().map(Path::to_path_buf));
                    merged.push_group(package).with_context(|| {
                        format!(
                            "failed to merge target dependency `{name}`",
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
                            "failed to merge world of target dependency `{name}`",
                            name = resolution.name()
                        )
                    })?;
                }
            }
        }

        let package = merged.push_group(root).with_context(|| {
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
                    "failed to select the default world to use for local target `{path}`. \
                     Please ensure that a world is specified in Cargo.toml under [package.metadata.component.target].",
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
                    for name in package.main.foreign_deps.keys() {
                        if !visiting.insert(name) {
                            bail!("foreign dependency `{name}` forms a dependency cycle while parsing target dependency `{other}`", other = resolution.name());
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
        let name = resolution.metadata.name.to_kebab_case();
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
            stability: Default::default(),
        });

        resolve.packages[package].worlds.insert(name, world);

        (resolve, world)
    }

    /// This function imports in the target world the exports of the source world.
    ///
    /// This is used for dependencies on other components so that their exports may
    /// be imported by the component being built.
    ///
    /// This also populates the import name map, which is used to map import names
    /// that the bindings supports to `unlocked-dep` import names used in the output
    /// component.
    fn import_world(
        resolve: &mut Resolve,
        source_id: WorldId,
        target_id: WorldId,
        version: Option<&Version>,
        import_name_map: &mut HashMap<String, String>,
    ) -> Result<()> {
        let mut functions = IndexMap::default();
        let mut used = IndexMap::new();
        let mut interfaces = IndexMap::new();

        // Check for directly used types from the component's world
        // Add any used interfaces to the `used` map
        for item in resolve.worlds[source_id].imports.values() {
            if let WorldItem::Type(ty) = &item {
                if let TypeDefKind::Type(Type::Id(ty)) = resolve.types[*ty].kind {
                    if let TypeOwner::Interface(id) = resolve.types[ty].owner {
                        log::debug!(
                            "importing interface `{iface}` for used type `{ty}`",
                            iface = resolve.id_of(id).as_deref().unwrap_or("<unnamed>"),
                            ty = resolve.types[ty].name.as_deref().unwrap_or("<unnamed>")
                        );

                        used.insert(
                            WorldKey::Interface(id),
                            WorldItem::Interface {
                                id,
                                stability: Default::default(),
                            },
                        );
                    }
                }
            }
        }

        // Add imports for all exported items
        for (key, item) in &resolve.worlds[source_id].exports {
            match item {
                WorldItem::Function(f) => {
                    log::debug!("importing function `{name}`", name = f.name);
                    functions.insert(key.clone().unwrap_name(), f.clone());
                }
                WorldItem::Interface { id, stability: _ } => {
                    let name = match key {
                        WorldKey::Name(name) => name.clone(),
                        WorldKey::Interface(id) => {
                            let iface = &resolve.interfaces[*id];
                            let name = iface.name.as_deref().expect("interface has no name");
                            match iface.package {
                                Some(pkg) => {
                                    let pkg = &resolve.packages[pkg];
                                    format!(
                                        "{ns}-{pkg}-{name}",
                                        ns = pkg.name.namespace,
                                        pkg = pkg.name.name
                                    )
                                }
                                None => name.to_string(),
                            }
                        }
                    };

                    // Check for used types from this interface
                    // Add any used interfaces to the `used` map
                    for (_, ty) in &resolve.interfaces[*id].types {
                        if let TypeDefKind::Type(Type::Id(ty)) = resolve.types[*ty].kind {
                            if let TypeOwner::Interface(other) = resolve.types[ty].owner {
                                if other != *id {
                                    log::debug!(
                                        "importing interface `{iface}` for used type `{ty}`",
                                        iface =
                                            resolve.id_of(other).as_deref().unwrap_or("<unnamed>"),
                                        ty = resolve.types[ty]
                                            .name
                                            .as_deref()
                                            .unwrap_or("<unnamed>")
                                    );

                                    used.insert(
                                        WorldKey::Interface(other),
                                        WorldItem::Interface {
                                            id: other,
                                            stability: Default::default(),
                                        },
                                    );
                                }
                            }
                        }
                    }

                    log::debug!(
                        "importing interface `{iface}`",
                        iface = resolve.id_of(*id).as_ref().unwrap_or(&name),
                    );
                    interfaces.insert(name, *id);
                }
                _ => continue,
            }
        }

        // Import the used interfaces
        resolve.worlds[target_id].imports.extend(used);

        // Import the exported interfaces
        for (name, id) in interfaces {
            // Alloc an interface that will just serve as a name
            // for the import.
            let package = resolve.worlds[source_id].package;
            let name_id = resolve.interfaces.alloc(Interface {
                name: Some(name.clone()),
                types: Default::default(),
                functions: Default::default(),
                docs: Default::default(),
                package,
                stability: Default::default(),
            });

            let import_name =
                format_dep_import(&resolve.packages[package.unwrap()], Some(&name), version);
            import_name_map.insert(resolve.id_of(name_id).unwrap(), import_name);

            if resolve.worlds[target_id]
                .imports
                .insert(
                    WorldKey::Interface(name_id),
                    WorldItem::Interface {
                        id,
                        stability: Default::default(),
                    },
                )
                .is_some()
            {
                let iface = &resolve.interfaces[id];
                let package = &resolve.packages[iface.package.expect("interface has no package")];
                let id = package
                    .name
                    .interface_id(iface.name.as_deref().expect("interface has no name"));
                bail!("cannot import dependency `{id}` because it conflicts with an import in the target world");
            }
        }

        // If the world had functions, insert an interface that contains them.
        if !functions.is_empty() {
            let source = &resolve.worlds[source_id];
            let package = &resolve.packages[source.package.unwrap()];
            let name = format!(
                "{ns}-{pkg}",
                ns = package.name.namespace,
                pkg = package.name.name
            );

            import_name_map.insert(name.clone(), format_dep_import(package, None, version));

            let interface = resolve.interfaces.alloc(Interface {
                name: Some(name.clone()),
                docs: source.docs.clone(),
                types: Default::default(),
                functions,
                package: source.package,
                stability: Default::default(),
            });

            // Add any types owned by the world to the interface
            for (id, ty) in resolve.types.iter() {
                if ty.owner == TypeOwner::World(source_id) {
                    resolve.interfaces[interface]
                        .types
                        .insert(ty.name.clone().expect("type should have a name"), id);
                }
            }

            // Finally, insert the interface into the target world
            if resolve.worlds[target_id]
                .imports
                .insert(
                    WorldKey::Name(name.clone()),
                    WorldItem::Interface {
                        id: interface,
                        stability: Default::default(),
                    },
                )
                .is_some()
            {
                bail!("cannot import dependency `{name}` because it conflicts with an import in the target world");
            }
        }

        Ok(())
    }
}
