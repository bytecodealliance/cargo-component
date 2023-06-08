//! Module for bindings generation.

use crate::{
    last_modified_time,
    metadata::{self, ComponentMetadata, Target},
    registry::{DecodedDependency, PackageDependencyResolution},
};
use anyhow::{bail, Context, Result};
use heck::{ToSnakeCase, ToUpperCamelCase};
use indexmap::{IndexMap, IndexSet};
use semver::Version;
use std::{
    collections::{HashMap, HashSet},
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
    Function, Interface, Package, PackageName, Resolve, Type, TypeDef, TypeDefKind, TypeId,
    TypeOwner, UnresolvedPackage, World, WorldId, WorldItem, WorldKey,
};

pub(crate) const BINDINGS_VERSION: &str = "0.1.0";
pub(crate) const WIT_BINDGEN_VERSION: &str = "0.6.0";

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

/// A generator for bindings crates.
pub struct BindingsGenerator<'a> {
    resolution: &'a PackageDependencyResolution,
    name: String,
    manifest_path: PathBuf,
    source_path: PathBuf,
    resolve: Resolve,
    world: WorldId,
    source_files: Vec<PathBuf>,
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

        let (resolve, world, source_files) = Self::create_target_world(resolution)?;

        Ok(Self {
            resolution,
            name,
            manifest_path,
            source_path,
            resolve,
            world,
            source_files,
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

    /// Encodes the target world used by the generator to a binary format.
    pub fn encode_target_world(&self) -> Result<Vec<u8>> {
        wit_component::encode(
            &self.resolve,
            self.resolve.worlds[self.world]
                .package
                .context("world has no package")?,
        )
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
"wit-bindgen" = {{ version = "{WIT_BINDGEN_VERSION}", features = ["realloc"], default-features = false }}
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
            // For now, set it to the package name from the id
            let world = &mut resolve.worlds[component_world_id];
            world.name = id.package_name().to_string();

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
        id: &metadata::Id,
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
        let mut unversioned: HashMap<_, Vec<_>> = HashMap::new();
        for (id, resolution) in &resolution.target_resolutions {
            let decoded = resolution.decode()?;
            let name = decoded.package_name();

            let versionless = PackageName {
                namespace: name.namespace.clone(),
                name: name.name.clone(),
                version: None,
            };

            let (index, prev) = deps.insert_full(name.clone(), decoded);
            if let Some(prev) = prev {
                bail!("duplicate definitions of package `{name}` found while decoding target dependency `{id}`", name = prev.package_name());
            }

            // We're storing the dependencies with versionless package names
            // This allows us to resolve a versionless foreign dependency to a singular
            // versioned dependency, if there is one
            unversioned.entry(versionless).or_default().push(index);
        }

        // Parse the target package itself
        let mut root = if path.is_dir() {
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
            visit(dep, &deps, &unversioned, &mut order, &mut visiting)?;
        }

        assert!(visiting.is_empty());

        // Merge all of the dependencies first
        let mut versions = HashMap::new();
        for name in order {
            let pkg = match deps.remove(&name).unwrap() {
                DecodedDependency::Wit {
                    resolution,
                    mut package,
                } => {
                    fixup_foreign_deps(&mut package, &versions);
                    source_files.extend(package.source_files().map(Path::to_path_buf));
                    merged.push(package).with_context(|| {
                        format!(
                            "failed to merge target dependency `{id}`",
                            id = resolution.id()
                        )
                    })?
                }
                DecodedDependency::Wasm {
                    resolution,
                    decoded,
                } => {
                    let (resolve, pkg) = match decoded {
                        DecodedWasm::WitPackage(resolve, pkg) => (resolve, pkg),
                        DecodedWasm::Component(resolve, world) => {
                            let pkg = resolve.worlds[world].package.unwrap();
                            (resolve, pkg)
                        }
                    };

                    merged
                        .merge(resolve)
                        .with_context(|| {
                            format!(
                                "failed to merge world of target dependency `{id}`",
                                id = resolution.id()
                            )
                        })?
                        .packages[pkg.index()]
                }
            };

            let pkg = &merged.packages[pkg];
            if let Some(version) = &pkg.name.version {
                versions
                    .entry(PackageName {
                        namespace: pkg.name.namespace.clone(),
                        name: pkg.name.name.clone(),
                        version: None,
                    })
                    .or_default()
                    .push(version.clone());
            }
        }

        fixup_foreign_deps(&mut root, &versions);

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

        fn fixup_foreign_deps(
            package: &mut UnresolvedPackage,
            versions: &HashMap<PackageName, Vec<Version>>,
        ) {
            package.foreign_deps = std::mem::take(&mut package.foreign_deps)
                .into_iter()
                .map(|(mut k, v)| {
                    match versions.get(&k) {
                        // Only assign the version if there's exactly one matching package
                        // Otherwise, let `wit-parser` handle the ambiguity
                        Some(versions) if versions.len() == 1 => {
                            k.version = Some(versions[0].clone());
                        }
                        _ => {}
                    }

                    (k, v)
                })
                .collect();
        }

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
                            bail!("foreign dependency `{name}` forms a dependency cycle while parsing target dependency `{id}`", id = resolution.id());
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

    fn target_empty_world(resolution: &PackageDependencyResolution) -> (Resolve, WorldId) {
        let mut resolve = Resolve::default();
        let name = resolution.metadata.name.clone();
        let package = resolve.packages.alloc(Package {
            name: PackageName {
                namespace: "component".to_string(),
                name: name.clone(),
                version: None,
            },
            interfaces: Default::default(),
            worlds: Default::default(),
        });

        let world = resolve.worlds.alloc(World {
            name,
            docs: Default::default(),
            imports: Default::default(),
            exports: Default::default(),
            package: Some(package),
        });

        (resolve, world)
    }

    // This function imports in the target world the exports of the source world.
    fn import_world(resolve: &mut Resolve, source: WorldId, target: WorldId) -> Result<()> {
        let mut types = IndexMap::default();
        let mut functions = IndexMap::default();
        let mut interfaces = IndexMap::new();
        let name;

        {
            let source = &resolve.worlds[source];
            name = source.name.clone();
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
            if target
                .imports
                .insert(key.clone(), WorldItem::Interface(id))
                .is_some()
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

fn export_trait_path(resolve: &Resolve, key: &WorldKey) -> String {
    match key {
        WorldKey::Name(n) => format!(
            "bindings::exports::{n}::{camel}",
            camel = n.to_upper_camel_case()
        ),
        WorldKey::Interface(id) => {
            let iface = &resolve.interfaces[*id];
            let pkg = &resolve.packages[iface.package.expect("interface should have a package")];
            let name = iface.name.as_deref().expect("unnamed interface");
            format!(
                "bindings::exports::{ns}::{pkg}::{name}::{camel}",
                ns = pkg.name.namespace,
                pkg = pkg.name.name,
                camel = name.to_upper_camel_case()
            )
        }
    }
}

/// Represents a Rust source code generator for targeting a given wit package.
///
/// The generated source defines a component that will implement the expected
/// export traits for the given world.
pub struct SourceGenerator<'a> {
    id: &'a metadata::Id,
    path: &'a Path,
    format: bool,
}

impl<'a> SourceGenerator<'a> {
    /// Creates a new source generator for the given path to
    /// a binary-encoded target wit package.
    ///
    /// If `format` is true, then `cargo fmt` will be run on the generated source.
    pub fn new(id: &'a metadata::Id, path: &'a Path, format: bool) -> Self {
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

        let mut export_count = 0;
        let mut function_exports = Vec::new();
        for (key, item) in &world.exports {
            match item {
                WorldItem::Function(f) => {
                    function_exports.push(f);
                }
                WorldItem::Interface(i) => {
                    let interface = &resolve.interfaces[*i];
                    writeln!(
                        &mut source,
                        "\nimpl {path} for Component {{",
                        path = export_trait_path(&resolve, key),
                    )
                    .unwrap();

                    for (i, (_, func)) in interface.functions.iter().enumerate() {
                        if i > 0 {
                            source.push('\n');
                        }
                        Self::print_unimplemented_func(&resolve, func, &mut source)?;
                        export_count += 1;
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
                Self::print_unimplemented_func(&resolve, func, &mut source)?;
                export_count += 1;
            }

            source.push_str("}\n");
        }

        if export_count > 0 {
            source.push_str("\nbindings::export!(Component);\n");
        }

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

        let decoded = wit_component::decode(&bytes).with_context(|| {
            format!(
                "failed to decode the content of target package `{id}` path `{path}`",
                id = self.id,
                path = self.path.display()
            )
        })?;

        match decoded {
            DecodedWasm::WitPackage(resolve, package) => {
                let world = resolve.select_world(package, world).with_context(|| {
                    format!(
                        "failed to select world from target package `{id}`",
                        id = self.id
                    )
                })?;
                Ok((resolve, world))
            }
            DecodedWasm::Component(..) => bail!("target is not a WIT package"),
        }
    }

    fn print_unimplemented_func(
        resolve: &Resolve,
        func: &Function,
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
            Self::print_type(resolve, param, source)?;
        }
        source.push(')');
        match func.results.len() {
            0 => {}
            1 => {
                source.push_str(" -> ");
                Self::print_type(resolve, func.results.iter_types().next().unwrap(), source)?;
            }
            _ => {
                source.push_str(" -> (");
                for (i, ty) in func.results.iter_types().enumerate() {
                    if i > 0 {
                        source.push_str(", ");
                    }
                    Self::print_type(resolve, ty, source)?;
                }
                source.push(')');
            }
        }
        source.push_str(" {\n        unimplemented!()\n    }\n");
        Ok(())
    }

    fn print_type(resolve: &Resolve, ty: &Type, source: &mut String) -> Result<()> {
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
            Type::Id(id) => Self::print_type_id(resolve, *id, source)?,
        }

        Ok(())
    }

    fn print_type_id(resolve: &Resolve, id: TypeId, source: &mut String) -> Result<()> {
        let ty = &resolve.types[id];

        if ty.name.is_some() {
            Self::print_type_path(resolve, ty, source);
            return Ok(());
        }

        match &ty.kind {
            TypeDefKind::List(ty) => {
                source.push_str("Vec<");
                Self::print_type(resolve, ty, source)?;
                source.push('>');
            }
            TypeDefKind::Option(ty) => {
                source.push_str("Option<");
                Self::print_type(resolve, ty, source)?;
                source.push('>');
            }
            TypeDefKind::Result(r) => {
                source.push_str("Result<");
                Self::print_optional_type(resolve, r.ok.as_ref(), source)?;
                source.push_str(", ");
                Self::print_optional_type(resolve, r.err.as_ref(), source)?;
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
                    Self::print_type(resolve, ty, source)?;
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
                Self::print_optional_type(resolve, ty.as_ref(), source)?;
                source.push('>');
            }
            TypeDefKind::Stream(stream) => {
                source.push_str("Stream<");
                Self::print_optional_type(resolve, stream.element.as_ref(), source)?;
                source.push_str(", ");
                Self::print_optional_type(resolve, stream.end.as_ref(), source)?;
                source.push('>');
            }
            TypeDefKind::Type(ty) => Self::print_type(resolve, ty, source)?,
            TypeDefKind::Unknown => unreachable!(),
        }

        Ok(())
    }

    fn print_type_path(resolve: &Resolve, ty: &TypeDef, source: &mut String) {
        let name = ty.name.as_ref().unwrap().to_upper_camel_case();

        if let TypeOwner::Interface(id) = ty.owner {
            if let Some(pkg) = resolve.interfaces[id].package {
                let name = &resolve.packages[pkg].name;
                write!(
                    source,
                    "bindings::{ns}::{pkg}::{name}",
                    ns = name.namespace,
                    pkg = name.name
                )
                .unwrap();
                return;
            }
        }

        write!(source, "bindings::{name}").unwrap();
    }

    fn print_optional_type(
        resolve: &Resolve,
        ty: Option<&Type>,
        source: &mut String,
    ) -> Result<()> {
        match ty {
            Some(ty) => Self::print_type(resolve, ty, source)?,
            None => source.push_str("()"),
        }

        Ok(())
    }
}
