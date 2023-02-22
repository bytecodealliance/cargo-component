//! Module for bindings generation.

use crate::{
    last_modified_time,
    metadata::{self, ComponentMetadata, Target},
    registry::PackageDependencyResolution,
};
use anyhow::{anyhow, bail, Context, Result};
use heck::ToSnakeCase;
use indexmap::IndexMap;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};
use wit_bindgen_gen_guest_rust::Opts;
use wit_component::DecodedWasm;
use wit_parser::{
    Document, Interface, InterfaceId, Package, Resolve, UnresolvedPackage, World, WorldId,
    WorldItem,
};

/// A generator for bindings crates.
pub struct BindingsGenerator<'a> {
    resolution: &'a PackageDependencyResolution,
    name: String,
    manifest_path: PathBuf,
    source_path: PathBuf,
}

impl<'a> BindingsGenerator<'a> {
    /// Creates a new bindings generator for the given bindings directory and package
    /// dependency resolution.
    pub fn new(
        bindings_dir: &'a Path,
        resolution: &'a PackageDependencyResolution,
    ) -> Result<Self> {
        let name = format!(
            "{name}-bindings",
            name = resolution.metadata.name.to_snake_case()
        );
        let package_dir = bindings_dir.join(&resolution.metadata.name);
        let manifest_path = package_dir.join("Cargo.toml");
        let source_path = package_dir.join("src").join("lib.rs");

        Ok(Self {
            resolution,
            name,
            manifest_path,
            source_path,
        })
    }

    /// Gets the cargo metadata for the package that the bindings are generated for.
    pub fn metadata(&self) -> &ComponentMetadata {
        &self.resolution.metadata
    }

    /// Gets the reason for generating the bindings.
    ///
    /// If this returns `Ok(None)`, then the bindings are up-to-date and
    /// do not need to be regenerated.
    ///
    ///
    /// If `force` is true, bindings generation will be forced even if the bindings are up-to-date.
    pub fn reason(
        &self,
        last_modified_exe: SystemTime,
        force: bool,
    ) -> Result<Option<&'static str>> {
        let last_modified_output = self
            .source_path
            .is_file()
            .then(|| last_modified_time(&self.source_path))
            .transpose()?
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let metadata = self.metadata();
        let manifest_modified = metadata.modified_at > last_modified_output;
        let exe_modified = last_modified_exe > last_modified_output;
        let target_modified = if let Some(Target::Local { path, .. }) = &metadata.section.target {
            last_modified_time(path)? > last_modified_output
        } else {
            false
        };

        if force
            || manifest_modified
            || exe_modified
            || target_modified
            || self.dependencies_are_newer(last_modified_output)?
        {
            Ok(Some(if force {
                "generation was forced"
            } else if manifest_modified {
                "the manifest was modified"
            } else if exe_modified {
                "the cargo-component executable was modified"
            } else if target_modified {
                "the target WIT file was modified"
            } else {
                "a dependency was modified"
            }))
        } else {
            Ok(None)
        }
    }

    /// Gets the name of the bindings package.
    pub fn package_name(&self) -> &str {
        &self.name
    }

    /// Gets the directory of the bindings package.
    pub fn package_dir(&self) -> &Path {
        self.manifest_path.parent().unwrap()
    }

    /// Generates the bindings
    pub fn generate(&self) -> Result<()> {
        let package_dir = self.package_dir();

        fs::create_dir_all(package_dir).with_context(|| {
            format!(
                "failed to create package bindings directory `{path}`",
                path = package_dir.display()
            )
        })?;

        self.create_manifest_file()?;
        self.create_source_file()?;

        Ok(())
    }

    fn create_manifest_file(&self) -> Result<()> {
        fs::write(
            &self.manifest_path,
            format!(
                r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
"wit-bindgen" = {{ version = "0.3.0", features = ["realloc"], default_features = false }}
"#,
                name = self.name
            ),
        )
        .with_context(|| {
            format!(
                "failed to create bindings package manifest `{path}`",
                path = self.manifest_path.display()
            )
        })
    }

    fn create_source_file(&self) -> Result<()> {
        let source_dir = self.source_path.parent().unwrap();
        fs::create_dir_all(source_dir).with_context(|| {
            format!(
                "failed to create source directory `{path}`",
                path = source_dir.display()
            )
        })?;

        let (resolve, world) = self.create_target_world()?;
        let opts = Opts {
            rustfmt: true,
            macro_export: true,
            macro_call_prefix: Some("bindings::".to_string()),
            export_macro_name: Some("export".to_string()),
            ..Default::default()
        };

        let mut files = Default::default();
        let mut generator = opts.build();
        generator.generate(&resolve, world, &mut files);

        fs::write(
            &self.source_path,
            files.iter().map(|(_, bytes)| bytes).next().unwrap_or(&[]),
        )
        .with_context(|| {
            format!(
                "failed to create source file `{path}`",
                path = self.source_path.display()
            )
        })?;

        Ok(())
    }

    fn dependencies_are_newer(&self, last_modified_output: SystemTime) -> Result<bool> {
        for (_, dep) in self.resolution.deps() {
            if last_modified_time(&dep.path)? > last_modified_output {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn create_target_world(&self) -> Result<(Resolve, WorldId)> {
        let (mut merged, world_id) = match &self.resolution.metadata.section.target {
            Some(Target::Package { package, world }) => {
                self.target_package(&package.id, world.as_deref())?
            }
            Some(Target::Local { path, world, .. }) => {
                self.target_local_file(path, world.as_deref())?
            }
            None => self.target_empty_world(),
        };

        // Merge all component dependencies as interface imports
        for (name, dependency) in &self.resolution.component_dependencies {
            match dependency.decode()? {
                DecodedWasm::WitPackage(_, _) => {
                    bail!("component dependency `{name}` is not a WebAssembly component")
                }
                DecodedWasm::Component(resolve, id) => {
                    let id = merged.merge(resolve).worlds[id.index()];
                    let interface = Self::import_world(&mut merged, id);
                    if merged.worlds[world_id]
                        .imports
                        .insert(name.to_string(), WorldItem::Interface(interface))
                        .is_some()
                    {
                        bail!("cannot import dependency `{name}` because it conflicts with an import in the target world");
                    }
                }
            }
        }

        Ok((merged, world_id))
    }

    fn target_package(
        &self,
        id: &metadata::PackageId,
        world: Option<&str>,
    ) -> Result<(Resolve, WorldId)> {
        // We must have resolved a target package dependency at this point
        assert_eq!(self.resolution.target_dependencies.len(), 1);

        // Decode the target package dependency
        let dependency = self.resolution.target_dependencies.values().next().unwrap();
        let decoded = dependency.decode()?;
        let package = decoded.package();
        let resolve = match decoded {
            DecodedWasm::WitPackage(resolve, _) => resolve,
            DecodedWasm::Component(resolve, _) => resolve,
        };

        // Currently, "default" isn't encodable for worlds, so try to
        // use `select_world` here, but otherwise fall back to a search
        let world = match resolve.select_world(package, world) {
            Ok(world) => world,
            Err(_) => {
                let (document, world) = match world {
                    Some(world) => world
                        .split_once('.')
                        .map(|(d, w)| (Some(d), Some(w)))
                        .unwrap_or((Some(world), None)),
                    None => (None, None),
                };

                // Resolve the document to use
                let package = &resolve.packages[package];
                let document = match document {
                    Some(name) => *package.documents.get(name).ok_or_else(|| {
                        anyhow!("target package `{id}` does not contain a document named `{name}`")
                    })?,
                    None if package.documents.len() == 1 => package.documents[0],
                    None if package.documents.len() > 1 => bail!("target package `{id}` contains multiple documents; specify the one to use with the `world` field in the manifest file"),
                    None => bail!("target package `{id}` contains no documents"),
                };

                // Resolve the world to use
                let document = &resolve.documents[document];
                match world {
                    Some(name) => *document.worlds.get(name).ok_or_else(|| {
                        anyhow!("target package `{id}` does not contain a world named `{name}` in document `{document}`", document = document.name)
                    })?,
                    None if document.default_world.is_some() => document.default_world.unwrap(),
                    None if document.worlds.len() == 1 => document.worlds[0],
                    None if document.worlds.len() > 1 => bail!("target document `{document}` in package `{id}` contains multiple worlds; specify the one to use with the `world` field in the manifest file", document = document.name),
                    None => bail!("target document `{document}` in package `{id}` contains no worlds", document = document.name),
                }
            }
        };

        Ok((resolve, world))
    }

    fn target_local_file(&self, path: &Path, world: Option<&str>) -> Result<(Resolve, WorldId)> {
        let mut merged = Resolve::default();

        // Start by decoding and merging all of the target dependencies
        let mut dependencies = HashMap::new();
        for (name, dependency) in &self.resolution.target_dependencies {
            let (resolve, package) = match dependency.decode()? {
                DecodedWasm::WitPackage(resolve, package) => (resolve, package),
                DecodedWasm::Component(..) => bail!("target dependency `{name}` is a WIT package"),
            };
            dependencies.insert(
                name.clone(),
                merged.merge(resolve).packages[package.index()],
            );
        }

        // Next parse the local target file, giving it the packages we just merged
        let package = merged
            .push(UnresolvedPackage::parse_file(path)?, &dependencies)
            .with_context(|| {
                format!(
                    "failed to parse local target `{path}`",
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

        Ok((merged, world))
    }

    fn target_empty_world(&self) -> (Resolve, WorldId) {
        let mut resolve = Resolve::default();
        let name = self.resolution.metadata.name.clone();
        let package = resolve.packages.alloc(Package {
            name: name.clone(),
            url: None,
            documents: Default::default(),
        });

        let document = resolve.documents.alloc(Document {
            name: name.clone(),
            interfaces: Default::default(),
            worlds: Default::default(),
            default_interface: None,
            default_world: None,
            package: Some(package),
        });

        let world = resolve.worlds.alloc(World {
            name: name.clone(),
            docs: Default::default(),
            imports: Default::default(),
            exports: Default::default(),
            document,
        });

        let pkg = &mut resolve.packages[package];
        pkg.documents.insert(name.clone(), document);

        let doc = &mut resolve.documents[document];
        doc.worlds.insert(name, world);
        doc.default_world = Some(world);

        (resolve, world)
    }

    // This function imports the exports of the given world as a new interface.
    fn import_world(resolve: &mut Resolve, id: WorldId) -> InterfaceId {
        let world = &resolve.worlds[id];
        let name = world.name.clone();
        let mut types = IndexMap::default();
        let mut functions = IndexMap::default();

        for (name, item) in &world.exports {
            match item {
                WorldItem::Function(f) => {
                    functions.insert(name.clone(), f.clone());
                }
                WorldItem::Interface(_) => continue,
                WorldItem::Type(t) => {
                    types.insert(name.clone(), *t);
                }
            }
        }

        resolve.interfaces.alloc(Interface {
            name: Some(name),
            docs: Default::default(),
            types,
            functions,
            document: world.document,
        })
    }
}
