//! Module for bindings generation.

use crate::{
    last_modified_time,
    metadata::{self, ComponentMetadata, Target},
    registry::PackageDependencyResolution,
};
use anyhow::{anyhow, bail, Context, Result};
use heck::{ToSnakeCase, ToUpperCamelCase};
use indexmap::IndexMap;
use std::{
    collections::HashMap,
    fmt::Write,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};
use wit_bindgen_rust::Opts;
use wit_bindgen_rust_lib::to_rust_ident;
use wit_component::DecodedWasm;
use wit_parser::{
    Document, Function, Interface, InterfaceId, Package, PackageId, Resolve, Type, TypeDef,
    TypeDefKind, TypeId, TypeOwner, UnresolvedPackage, World, WorldId, WorldItem,
};

pub(crate) const BINDINGS_VERSION: &str = "0.1.0";

fn select_world(
    resolve: &Resolve,
    id: &metadata::PackageId,
    package: PackageId,
    world: Option<&str>,
) -> Result<WorldId> {
    // Currently, "default" isn't encodable for worlds, so try to
    // use `select_world` here, but otherwise fall back to a search
    match resolve.select_world(package, world) {
        Ok(world) => Ok(world),
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
                Some(name) => Ok(*document.worlds.get(name).ok_or_else(|| {
                    anyhow!("target package `{id}` does not contain a world named `{name}` in document `{document}`", document = document.name)
                })?),
                None if document.default_world.is_some() => Ok(document.default_world.unwrap()),
                None if document.worlds.len() == 1 => Ok(document.worlds[0]),
                None if document.worlds.len() > 1 => bail!("target document `{document}` in package `{id}` contains multiple worlds; specify the one to use with the `world` field in the manifest file", document = document.name),
                None => bail!("target document `{document}` in package `{id}` contains no worlds", document = document.name),
            }
        }
    }
}

/// A generator for bindings crates.
pub struct BindingsGenerator<'a> {
    resolution: &'a PackageDependencyResolution,
    name: String,
    manifest_path: PathBuf,
    source_path: PathBuf,
    resolve: Resolve,
    world: WorldId,
    deps: Vec<PathBuf>,
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

        let (resolve, world, deps) = Self::create_target_world(resolution)?;

        Ok(Self {
            resolution,
            name,
            manifest_path,
            source_path,
            resolve,
            world,
            deps,
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
version = "{BINDINGS_VERSION}"
edition = "2021"
publish = false

[dependencies]
"wit-bindgen" = {{ version = "0.4.0", features = ["realloc"], default-features = false }}
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

        let opts = Opts {
            rustfmt: true,
            macro_export: true,
            macro_call_prefix: Some("bindings::".to_string()),
            export_macro_name: Some("export".to_string()),
            ..Default::default()
        };

        let mut files = Default::default();
        let mut generator = opts.build();
        generator.generate(&self.resolve, self.world, &mut files);

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
        for dep in &self.deps {
            if last_modified_time(dep)? > last_modified_output {
                log::debug!(
                    "dependency `{path}` has been modified",
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
        let (mut merged, world_id, deps) = match &resolution.metadata.section.target {
            Some(Target::Package { package, world }) => {
                let (merged, world) =
                    Self::target_package(resolution, &package.id, world.as_deref())?;
                (merged, world, Vec::new())
            }
            Some(Target::Local { path, world, .. }) => {
                Self::target_local_file(resolution, path, world.as_deref())?
            }
            None => {
                let (merged, world) = Self::target_empty_world(resolution);
                (merged, world, Vec::new())
            }
        };

        // Merge all component dependencies as interface imports
        for (name, dependency) in &resolution.resolutions {
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

        Ok((merged, world_id, deps))
    }

    fn target_package(
        resolution: &PackageDependencyResolution,
        id: &metadata::PackageId,
        world: Option<&str>,
    ) -> Result<(Resolve, WorldId)> {
        // We must have resolved a target package dependency at this point
        assert_eq!(resolution.target_resolutions.len(), 1);

        // Decode the target package dependency
        let dependency = resolution.target_resolutions.values().next().unwrap();
        let decoded = dependency.decode()?;
        let package = decoded.package();
        let resolve = match decoded {
            DecodedWasm::WitPackage(resolve, _) => resolve,
            DecodedWasm::Component(resolve, _) => resolve,
        };

        let world = select_world(&resolve, id, package, world)?;
        Ok((resolve, world))
    }

    fn target_local_file(
        resolution: &PackageDependencyResolution,
        path: &Path,
        world: Option<&str>,
    ) -> Result<(Resolve, WorldId, Vec<PathBuf>)> {
        let mut merged = Resolve::default();

        // Start by decoding and merging all of the target dependencies
        let mut dependencies = HashMap::new();
        for (name, dependency) in &resolution.target_resolutions {
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
        let (package, deps) = if path.is_dir() {
            merged.push_dir(path)?
        } else {
            (
                merged
                    .push(UnresolvedPackage::parse_file(path)?, &dependencies)
                    .with_context(|| {
                        format!(
                            "failed to parse local target `{path}`",
                            path = path.display()
                        )
                    })?,
                Vec::new(),
            )
        };

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

        Ok((merged, world, deps))
    }

    fn target_empty_world(resolution: &PackageDependencyResolution) -> (Resolve, WorldId) {
        let mut resolve = Resolve::default();
        let name = resolution.metadata.name.clone();
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

/// Represents a Rust source code generator for targeting a given wit package.
///
/// The generated source defines a component that will implement the expected
/// export traits for the given world.
pub struct SourceGenerator<'a> {
    id: &'a metadata::PackageId,
    path: &'a Path,
    format: bool,
}

impl<'a> SourceGenerator<'a> {
    /// Creates a new source generator for the given path to
    /// a binary-encoded target wit package.
    ///
    /// If `format` is true, then `cargo fmt` will be run on the generated source.
    pub fn new(id: &'a metadata::PackageId, path: &'a Path, format: bool) -> Self {
        Self { id, path, format }
    }

    /// Generates the Rust source code for the given world.
    pub fn generate(&self, world: Option<&str>) -> Result<String> {
        let (resolve, world) = self.decode(world)?;
        let mut source = String::new();
        let world = &resolve.worlds[world];

        source.push_str("struct Component;\n");
        if world.exports.is_empty() {
            return Ok(source);
        }

        let interface_names = world
            .exports
            .iter()
            .chain(world.imports.iter())
            .filter_map(|(name, item)| {
                if let WorldItem::Interface(i) = item {
                    Some((*i, to_rust_ident(name)))
                } else {
                    None
                }
            })
            .collect::<HashMap<_, _>>();

        let mut function_exports = Vec::new();
        for (name, item) in &world.exports {
            match item {
                WorldItem::Function(f) => {
                    function_exports.push(f);
                }
                WorldItem::Interface(i) => {
                    let interface = &resolve.interfaces[*i];
                    writeln!(
                        &mut source,
                        "\nimpl bindings::{module}::{name} for Component {{",
                        module = to_rust_ident(name),
                        name = name.to_upper_camel_case(),
                    )
                    .unwrap();

                    for (i, (_, func)) in interface.functions.iter().enumerate() {
                        if i > 0 {
                            source.push('\n');
                        }
                        Self::print_unimplemented_func(
                            &resolve,
                            func,
                            &interface_names,
                            &mut source,
                        )?;
                    }

                    source.push_str("}\n");
                }
                WorldItem::Type(_) => continue,
            }
        }

        if !function_exports.is_empty() {
            writeln!(
                &mut source,
                "\nimpl bindings::{name} for Component {{",
                name = world.name.to_upper_camel_case(),
            )
            .unwrap();

            for (i, func) in function_exports.iter().enumerate() {
                if i > 0 {
                    source.push('\n');
                }
                Self::print_unimplemented_func(&resolve, func, &interface_names, &mut source)?;
            }

            source.push_str("}\n");
        }

        source.push_str("\nbindings::export!(Component);\n");

        if self.format {
            let mut child = Command::new("rustfmt")
                .arg("--edition=2018")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .context("failed to spawn `rustfmt`")?;
            std::io::Write::write_all(&mut child.stdin.take().unwrap(), source.as_bytes())
                .context("failed to write to `rustfmt`")?;
            source.truncate(0);
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut source)
                .context("failed to write to `rustfmt`")?;
            let status = child.wait().context("failed to wait for `rustfmt`")?;
            if !status.success() {
                bail!("execution of `rustfmt` returned a non-zero exit code {status}");
            }
        }

        Ok(source)
    }

    fn decode(&self, world: Option<&str>) -> Result<(Resolve, WorldId)> {
        let bytes = fs::read(self.path).with_context(|| {
            format!(
                "failed to read the content of target package `{id}` path `{path}`",
                id = self.id,
                path = self.path.display()
            )
        })?;

        let decoded = wit_component::decode("target", &bytes).with_context(|| {
            format!(
                "failed to decode the content of target package `{id}` path `{path}`",
                id = self.id,
                path = self.path.display()
            )
        })?;

        match decoded {
            DecodedWasm::WitPackage(resolve, package) => {
                let world = select_world(&resolve, self.id, package, world)?;
                Ok((resolve, world))
            }
            DecodedWasm::Component(..) => bail!("target is not a WIT package"),
        }
    }

    fn print_unimplemented_func(
        resolve: &Resolve,
        func: &Function,
        interface_names: &HashMap<InterfaceId, String>,
        source: &mut String,
    ) -> Result<()> {
        // TODO: it would be nice to share the printing of the signature of the function
        // with wit-bindgen, but right now it's tightly coupled with interface generation.
        write!(source, "    fn {name}(", name = to_rust_ident(&func.name)).unwrap();
        for (i, (name, param)) in func.params.iter().enumerate() {
            if i > 0 {
                source.push_str(", ");
            }
            source.push_str(&to_rust_ident(name));
            source.push_str(": ");
            Self::print_type(resolve, param, interface_names, source)?;
        }
        source.push(')');
        match func.results.len() {
            0 => {}
            1 => {
                source.push_str(" -> ");
                Self::print_type(
                    resolve,
                    func.results.iter_types().next().unwrap(),
                    interface_names,
                    source,
                )?;
            }
            _ => {
                source.push_str(" -> (");
                for (i, ty) in func.results.iter_types().enumerate() {
                    if i > 0 {
                        source.push_str(", ");
                    }
                    Self::print_type(resolve, ty, interface_names, source)?;
                }
                source.push(')');
            }
        }
        source.push_str(" {\n        unimplemented!()\n    }\n");
        Ok(())
    }

    fn print_type(
        resolve: &Resolve,
        ty: &Type,
        interface_names: &HashMap<InterfaceId, String>,
        source: &mut String,
    ) -> Result<()> {
        match ty {
            Type::Bool => source.push_str("bool"),
            Type::U8 => source.push_str("u8"),
            Type::U16 => source.push_str("u16"),
            Type::U32 => source.push_str("u32"),
            Type::U64 => source.push_str("u64"),
            Type::S8 => source.push_str("i8"),
            Type::S16 => source.push_str("i16"),
            Type::S32 => source.push_str("i32"),
            Type::S64 => source.push_str("i64"),
            Type::Float32 => source.push_str("f32"),
            Type::Float64 => source.push_str("f64"),
            Type::Char => source.push_str("char"),
            Type::String => source.push_str("String"),
            Type::Id(id) => Self::print_type_id(resolve, *id, interface_names, source)?,
        }

        Ok(())
    }

    fn print_type_id(
        resolve: &Resolve,
        id: TypeId,
        interface_names: &HashMap<InterfaceId, String>,
        source: &mut String,
    ) -> Result<()> {
        let ty = &resolve.types[id];

        if ty.name.is_some() {
            Self::print_type_path(ty, interface_names, source);
            return Ok(());
        }

        match &ty.kind {
            TypeDefKind::List(ty) => {
                source.push_str("Vec<");
                Self::print_type(resolve, ty, interface_names, source)?;
                source.push('>');
            }
            TypeDefKind::Option(ty) => {
                source.push_str("Option<");
                Self::print_type(resolve, ty, interface_names, source)?;
                source.push('>');
            }
            TypeDefKind::Result(r) => {
                source.push_str("Result<");
                Self::print_optional_type(resolve, r.ok.as_ref(), interface_names, source)?;
                source.push_str(", ");
                Self::print_optional_type(resolve, r.err.as_ref(), interface_names, source)?;
                source.push('>');
            }
            TypeDefKind::Variant(_) => {
                bail!("unsupported anonymous variant type found in WIT package")
            }
            TypeDefKind::Tuple(t) => {
                source.push('(');
                for (i, ty) in t.types.iter().enumerate() {
                    if i > 0 {
                        source.push_str(", ");
                    }
                    Self::print_type(resolve, ty, interface_names, source)?;
                }
                source.push(')');
            }
            TypeDefKind::Record(_) => {
                bail!("unsupported anonymous record type found in WIT package")
            }
            TypeDefKind::Flags(_) => {
                bail!("unsupported anonymous flags type found in WIT package")
            }
            TypeDefKind::Enum(_) => {
                bail!("unsupported anonymous enum type found in WIT package")
            }
            TypeDefKind::Union(_) => {
                bail!("unsupported anonymous union type found in WIT package")
            }
            TypeDefKind::Future(ty) => {
                source.push_str("Future<");
                Self::print_optional_type(resolve, ty.as_ref(), interface_names, source)?;
                source.push('>');
            }
            TypeDefKind::Stream(stream) => {
                source.push_str("Stream<");
                Self::print_optional_type(
                    resolve,
                    stream.element.as_ref(),
                    interface_names,
                    source,
                )?;
                source.push_str(", ");
                Self::print_optional_type(resolve, stream.end.as_ref(), interface_names, source)?;
                source.push('>');
            }
            TypeDefKind::Type(ty) => Self::print_type(resolve, ty, interface_names, source)?,
            TypeDefKind::Unknown => unreachable!(),
        }

        Ok(())
    }

    fn print_type_path(
        ty: &TypeDef,
        interface_names: &HashMap<InterfaceId, String>,
        source: &mut String,
    ) {
        let name = ty.name.as_ref().unwrap().to_upper_camel_case();

        if let TypeOwner::Interface(id) = ty.owner {
            if let Some(path) = interface_names.get(&id) {
                write!(source, "bindings::{path}::{name}").unwrap();
                return;
            }
        }

        write!(source, "bindings::{name}").unwrap();
    }

    fn print_optional_type(
        resolve: &Resolve,
        ty: Option<&Type>,
        interface_names: &HashMap<InterfaceId, String>,
        source: &mut String,
    ) -> Result<()> {
        match ty {
            Some(ty) => Self::print_type(resolve, ty, interface_names, source)?,
            None => source.push_str("()"),
        }

        Ok(())
    }
}
