//! A module for implementing the Rust source generator used for
//! the `--target` option of the `new` command.
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Write},
    io::Read,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};
use cargo_component_core::registry::DependencyResolution;
use heck::{AsSnakeCase, ToSnakeCase, ToUpperCamelCase};
use indexmap::{map::Entry, IndexMap, IndexSet};
use wasm_pkg_client::PackageRef;
use wit_bindgen_rust::to_rust_ident;
use wit_parser::{
    Function, FunctionKind, Handle, Interface, Resolve, Type, TypeDef, TypeDefKind, TypeId,
    TypeOwner, World, WorldId, WorldItem, WorldKey,
};

/// The type name that implements the export traits.
const IMPLEMENTER: &str = "Component";

/// Represents a node in a "use" trie.
#[derive(Default)]
struct UseTrieNode {
    // Map of child path segment to trie node
    children: BTreeMap<String, UseTrieNode>,
    // Set of types that are used at this node
    tys: BTreeSet<String>,
}

impl fmt::Display for UseTrieNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.children.len() + self.tys.len() > 1 {
            write!(f, "{{")?;
        }

        // Print the children first
        for (i, (segment, child)) in self.children.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }

            write!(f, "{segment}::{child}")?;
        }

        if !self.children.is_empty() && !self.tys.is_empty() {
            write!(f, ", ")?;
        }

        // Next, print the types at this node
        for (i, ty) in self.tys.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }

            write!(f, "{ty}")?;
        }

        if self.children.len() + self.tys.len() > 1 {
            write!(f, "}}")?;
        }

        Ok(())
    }
}

/// A trie that maintains uses of types when generating Rust source code.
///
/// This is used to coalesce use statements similar to `rustfmt`, but
/// without relying on `rustfmt` to do so.
#[derive(Default)]
struct UseTrie {
    /// The root trie node.
    root: UseTrieNode,
    /// The set of all known path segments.
    segments: IndexSet<String>,
    /// The set of all known type names.
    types: IndexSet<String>,
}

impl UseTrie {
    /// Reserves names in the trie.
    ///
    /// Any conflicting insert into the tree will use a qualified path instead.
    fn reserve_names(&mut self, names: &ReservedNames) {
        for (name, count) in &names.0 {
            for i in 0..*count {
                let name = if i > 0 {
                    format!("{name}{i}", i = i + 1)
                } else {
                    name.clone()
                };

                self.types.insert(name);
            }
        }
    }

    /// Gets the used types at a given path.
    fn get<'a>(&self, path: impl Iterator<Item = &'a str>) -> Option<impl Iterator<Item = &str>> {
        let mut node = &self.root;
        for segment in path {
            node = node.children.get(segment)?;
        }

        Some(node.tys.iter().map(|ty| ty.as_str()))
    }

    /// Inserts a new use of the given type.
    ///
    /// This method handles the proper casing for path segments and type names.
    ///
    /// Returns the string to use when printing the type reference.
    fn insert<'a, I>(&mut self, path: I, ty: &str) -> Cow<str>
    where
        I: IntoIterator<Item = &'a str>,
        I::IntoIter: Clone,
    {
        let (type_index, inserted) = self.types.insert_full(ty.to_upper_camel_case());
        let ty: &String = &self.types[type_index];
        if !inserted {
            let path = path.into_iter();

            // Check to see if the type is already used at this path
            if let Some(tys) = self.get(path.clone()) {
                for existing in tys {
                    if ty == existing {
                        // Same path, so just return the type name
                        return ty.into();
                    }
                }
            }

            // Type conflicts with an existing type, so use the qualified type name
            return format!(
                "{path}::{ty}",
                path = path.enumerate().fold(String::new(), |mut s, (i, p)| {
                    if i > 0 {
                        s.push_str("::");
                    }
                    write!(s, "{p}", p = AsSnakeCase(p)).unwrap();
                    s
                }),
                ty = self.types[type_index],
            )
            .into();
        }

        let mut node = &mut self.root;
        for segment in path {
            assert!(!segment.is_empty());
            let (segment_index, _) = self.segments.insert_full(segment.to_snake_case());
            let segment = &self.segments[segment_index];
            node = node.children.entry(segment.clone()).or_default();
        }

        let inserted = node.tys.insert(ty.clone());
        assert!(inserted);

        // Return just the type name as we were able to use this type unqualified
        Cow::Borrowed(&self.types[type_index])
    }

    /// Inserts a type from a WIT interface.
    fn insert_interface_type(
        &mut self,
        resolve: &Resolve,
        interface: &Interface,
        ty: &str,
    ) -> Cow<str> {
        let pkg = &resolve.packages[interface.package.expect("interface should have a package")];
        let name = interface.name.as_deref().expect("unnamed interface");

        self.insert(
            [
                "bindings",
                "exports",
                pkg.name.namespace.as_str(),
                pkg.name.name.as_str(),
                name,
            ],
            ty,
        )
    }

    /// Inserts an export trait for the given world key.
    fn insert_export_trait(&mut self, resolve: &Resolve, key: &WorldKey) -> Cow<str> {
        match key {
            WorldKey::Name(name) => self.insert(["bindings", "exports", name.as_str()], "Guest"),
            WorldKey::Interface(id) => {
                let iface = &resolve.interfaces[*id];
                self.insert_interface_type(resolve, iface, "Guest")
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.root.children.is_empty() && self.root.tys.is_empty()
    }
}

impl fmt::Display for UseTrie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        assert!(self.root.tys.is_empty());
        for (segment, child) in &self.root.children {
            writeln!(f, "use {segment}::{child};")?;
        }

        Ok(())
    }
}

/// Used to keep track of type names for implementing types.
///
/// Conflicting type names will have a number appended to them.
#[derive(Default)]
struct ReservedNames(IndexMap<String, usize>);

impl ReservedNames {
    fn reserve(&mut self, name: &str) -> String {
        let mut name = name.to_upper_camel_case();
        let count = self.0.entry(name.clone()).or_insert(0);
        *count += 1;

        if *count > 1 {
            write!(&mut name, "{count}").unwrap();
        }

        name
    }
}

/// Used to write an unimplemented trait function.
struct UnimplementedFunction<'a> {
    resolve: &'a Resolve,
    func: &'a Function,
    target_world: &'a World,
}

impl<'a> UnimplementedFunction<'a> {
    fn new(resolve: &'a Resolve, func: &'a Function, target_world: &'a World) -> Self {
        Self {
            resolve,
            func,
            target_world,
        }
    }

    fn print(&self, trie: &mut UseTrie, source: &mut String) -> Result<()> {
        let (name, self_param, constructor) = match self.func.kind {
            FunctionKind::Freestanding => {
                (Cow::Owned(to_rust_ident(&self.func.name)), false, false)
            }
            FunctionKind::Method(_) => (
                to_rust_ident(
                    self.func
                        .name
                        .split_once('.')
                        .expect("invalid method name")
                        .1,
                )
                .into(),
                true,
                false,
            ),
            FunctionKind::Static(_) => (
                to_rust_ident(
                    self.func
                        .name
                        .split_once('.')
                        .expect("invalid method name")
                        .1,
                )
                .into(),
                false,
                false,
            ),
            FunctionKind::Constructor(_) => ("new".into(), false, true),
        };

        // TODO: it would be nice to share the printing of the signature of the function
        // with wit-bindgen, but right now it's tightly coupled with interface generation.
        write!(source, "    fn {name}(")?;

        for (i, (name, param)) in self.func.params.iter().enumerate() {
            if i > 0 {
                source.push_str(", ");
            }

            if i == 0 && self_param {
                write!(source, "&self")?;
            } else {
                source.push_str(&to_rust_ident(name));
                source.push_str(": ");
                self.print_type(param, trie, source)?;
            }
        }
        source.push(')');
        match self.func.results.len() {
            0 => {}
            1 => {
                source.push_str(" -> ");
                if constructor {
                    source.push_str("Self");
                } else {
                    self.print_type(self.func.results.iter_types().next().unwrap(), trie, source)?;
                }
            }
            _ => {
                source.push_str(" -> (");
                for (i, ty) in self.func.results.iter_types().enumerate() {
                    if i > 0 {
                        source.push_str(", ");
                    }

                    self.print_type(ty, trie, source)?;
                }

                source.push(')');
            }
        }
        source.push_str(" {\n        unimplemented!()\n    }\n");
        Ok(())
    }

    fn print_type(&self, ty: &Type, trie: &mut UseTrie, source: &mut String) -> Result<()> {
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
            Type::F32 => source.push_str("f32"),
            Type::F64 => source.push_str("f64"),
            Type::Char => source.push_str("char"),
            Type::String => source.push_str("String"),
            Type::Id(id) => self.print_type_id(*id, trie, source, false)?,
        }

        Ok(())
    }

    fn print_type_id(
        &self,
        id: TypeId,
        trie: &mut UseTrie,
        source: &mut String,
        type_name_borrow_suffix: bool,
    ) -> Result<()> {
        let ty = &self.resolve.types[id];

        if ty.name.is_some() {
            self.print_type_path(ty, trie, source, type_name_borrow_suffix);
            return Ok(());
        }

        match &ty.kind {
            TypeDefKind::List(ty) => {
                source.push_str("Vec<");
                self.print_type(ty, trie, source)?;
                source.push('>');
            }
            TypeDefKind::Option(ty) => {
                source.push_str("Option<");
                self.print_type(ty, trie, source)?;
                source.push('>');
            }
            TypeDefKind::Result(r) => {
                source.push_str("Result<");
                self.print_optional_type(r.ok.as_ref(), trie, source)?;
                source.push_str(", ");
                self.print_optional_type(r.err.as_ref(), trie, source)?;
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
                    self.print_type(ty, trie, source)?;
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
            TypeDefKind::Future(ty) => {
                source.push_str("Future<");
                self.print_optional_type(ty.as_ref(), trie, source)?;
                source.push('>');
            }
            TypeDefKind::Stream(stream) => {
                source.push_str("Stream<");
                self.print_optional_type(stream.element.as_ref(), trie, source)?;
                source.push_str(", ");
                self.print_optional_type(stream.end.as_ref(), trie, source)?;
                source.push('>');
            }
            TypeDefKind::Type(ty) => self.print_type(ty, trie, source)?,
            TypeDefKind::Handle(Handle::Own(id)) => self.print_type_id(*id, trie, source, false)?,
            TypeDefKind::Handle(Handle::Borrow(id)) => {
                let exported_resource = self.is_exported_resource(*id);
                if !exported_resource {
                    source.push('&');
                }
                self.print_type_id(*id, trie, source, exported_resource)?;
            }
            TypeDefKind::Resource => {
                bail!("unsupported anonymous resource type found in WIT package")
            }
            TypeDefKind::Unknown => unreachable!(),
        }

        Ok(())
    }

    fn is_exported_resource(&self, id: TypeId) -> bool {
        let type_info = &self.resolve.types[id];
        if let TypeOwner::World(_) = type_info.owner {
            return false;
        }
        match type_info.kind {
            TypeDefKind::Type(Type::Id(kind_id)) => {
                let kind_type = &self.resolve.types[kind_id];
                match kind_type.owner {
                    TypeOwner::World(_) => false,
                    TypeOwner::Interface(interface_id) => {
                        self.target_world
                            .exports
                            .values()
                            .any(|world_item| match world_item {
                                WorldItem::Interface(id) => *id == interface_id,
                                _ => false,
                            })
                    }
                    _ => true,
                }
            }
            TypeDefKind::Resource => match type_info.owner {
                TypeOwner::World(_) => false,
                TypeOwner::Interface(interface_id) => {
                    self.target_world
                        .exports
                        .values()
                        .any(|world_item| match world_item {
                            WorldItem::Interface(id) => *id == interface_id,
                            _ => false,
                        })
                }
                _ => true,
            },
            _ => false,
        }
    }

    fn print_type_path(
        &self,
        ty: &TypeDef,
        trie: &mut UseTrie,
        source: &mut String,
        type_name_borrow_suffix: bool,
    ) {
        // add the 'Borrow' suffix to the type name, if needed.
        // this is to match the wit-bindgen-rust bindings.
        let type_name = if type_name_borrow_suffix {
            format!("{}Borrow", ty.name.as_deref().unwrap())
        } else {
            ty.name.as_deref().unwrap().to_string()
        };

        if let TypeOwner::Interface(id) = ty.owner {
            let interface = &self.resolve.interfaces[id];
            if interface.package.is_some() {
                let name = trie.insert_interface_type(self.resolve, interface, &type_name);
                write!(source, "{name}",).unwrap();
                return;
            }
        }

        write!(
            source,
            "{name}",
            name = trie.insert(["bindings"], &type_name)
        )
        .unwrap();
    }

    fn print_optional_type(
        &self,
        ty: Option<&Type>,
        trie: &mut UseTrie,
        source: &mut String,
    ) -> Result<()> {
        match ty {
            Some(ty) => self.print_type(ty, trie, source)?,
            None => source.push_str("()"),
        }

        Ok(())
    }
}

/// Information about a resource type.
struct Resource<'a> {
    ty: &'a TypeDef,
    impl_name: String,
    functions: Vec<&'a Function>,
}

/// A generator for implementing the interface exports of a world.
struct InterfaceGenerator<'a> {
    resolve: &'a Resolve,
    key: &'a WorldKey,
    interface: &'a Interface,
    functions: Vec<&'a Function>,
    resources: IndexMap<TypeId, Resource<'a>>,
    target_world: &'a World,
}

impl<'a> InterfaceGenerator<'a> {
    fn new(
        resolve: &'a Resolve,
        key: &'a WorldKey,
        interface: &'a Interface,
        names: &mut ReservedNames,
        target_world: &'a World,
    ) -> Self {
        let mut functions = Vec::new();
        let mut resources: IndexMap<_, Resource> = IndexMap::new();

        // Search for resource-related functions in this interface
        for (_, func) in interface.functions.iter() {
            let id = match func.kind {
                FunctionKind::Freestanding => {
                    functions.push(func);
                    continue;
                }
                FunctionKind::Method(id)
                | FunctionKind::Static(id)
                | FunctionKind::Constructor(id) => id,
            };

            // Create a resource entry for this resource
            match resources.entry(id) {
                Entry::Occupied(mut entry) => entry.get_mut().functions.push(func),
                Entry::Vacant(entry) => {
                    let ty = &resolve.types[id];
                    let name = ty.name.as_deref().expect("unnamed resource type");
                    let impl_name = names.reserve(name);

                    entry.insert(Resource {
                        ty,
                        impl_name,
                        functions: vec![func],
                    });
                }
            }
        }

        // Add resources that did not have any methods
        for (_, id) in interface.types.iter() {
            let ty = &resolve.types[*id];
            if ty.kind == TypeDefKind::Resource && !resources.contains_key(id) {
                let name = ty.name.as_deref().expect("unnamed resource type");
                let impl_name = names.reserve(name);
                resources.insert(
                    *id,
                    Resource {
                        ty,
                        impl_name,
                        functions: vec![],
                    },
                );
            }
        }

        Self {
            resolve,
            key,
            interface,
            functions,
            resources,
            target_world,
        }
    }

    fn generate(&self, trie: &mut UseTrie) -> Result<String> {
        let mut source: String = String::new();

        for resource in self.resources.values() {
            writeln!(
                &mut source,
                "struct {impl_name};\n\nimpl {impl_trait} for {impl_name} {{",
                impl_name = resource.impl_name,
                impl_trait = trie.insert_interface_type(
                    self.resolve,
                    self.interface,
                    &format!("guest-{name}", name = resource.ty.name.as_deref().unwrap())
                )
            )?;

            for func in &resource.functions {
                UnimplementedFunction::new(self.resolve, func, self.target_world)
                    .print(trie, &mut source)?;
            }

            source.push_str("}\n");
        }

        if !self.resources.is_empty() {
            source.push('\n');
        }

        writeln!(
            &mut source,
            "impl {name} for {IMPLEMENTER} {{",
            name = trie.insert_export_trait(self.resolve, self.key),
        )?;

        for resource in self.resources.values() {
            writeln!(
                &mut source,
                "    type {name} = {impl_name};",
                name = resource
                    .ty
                    .name
                    .as_deref()
                    .expect("unnamed resource type")
                    .to_upper_camel_case(),
                impl_name = resource.impl_name,
            )?;
        }

        if !self.resources.is_empty() && !self.functions.is_empty() {
            source.push('\n');
        }

        for (i, func) in self.functions.iter().enumerate() {
            if i > 0 {
                source.push('\n');
            }

            UnimplementedFunction::new(self.resolve, func, self.target_world)
                .print(trie, &mut source)?;
        }

        source.push_str("}\n");
        Ok(source)
    }
}

/// A generator for implementing the export traits of a world.
struct ImplementationGenerator<'a> {
    resolve: &'a Resolve,
    functions: Vec<&'a Function>,
    interfaces: Vec<InterfaceGenerator<'a>>,
    target_world: &'a World,
}

impl<'a> ImplementationGenerator<'a> {
    fn new(resolve: &'a Resolve, world: &'a World, names: &mut ReservedNames) -> Self {
        let mut functions = Vec::new();
        let mut interfaces = Vec::new();

        for (key, item) in &world.exports {
            match item {
                WorldItem::Function(f) => {
                    functions.push(f);
                }
                WorldItem::Interface(iface) => {
                    let interface = &resolve.interfaces[*iface];
                    interfaces.push(InterfaceGenerator::new(
                        resolve, key, interface, names, world,
                    ));
                }
                WorldItem::Type(_) => continue,
            }
        }

        Self {
            resolve,
            functions,
            interfaces,
            target_world: world,
        }
    }

    fn generate(&self, trie: &mut UseTrie) -> Result<Vec<String>> {
        let mut impls = Vec::new();
        if !self.functions.is_empty() {
            let mut source = String::new();

            writeln!(
                &mut source,
                "\nimpl {name} for {IMPLEMENTER} {{",
                name = trie.insert(["bindings"], "Guest")
            )?;

            for (i, func) in self.functions.iter().enumerate() {
                if i > 0 {
                    source.push('\n');
                }

                UnimplementedFunction::new(self.resolve, func, self.target_world)
                    .print(trie, &mut source)?;
            }

            source.push_str("}\n");
            impls.push(source);
        }

        for interface in &self.interfaces {
            impls.push(interface.generate(trie)?);
        }

        Ok(impls)
    }
}

/// Represents a Rust source code generator for targeting a given WIT package.
///
/// The generated source defines a component that will implement the expected
/// export traits for the given world.
pub struct SourceGenerator<'a> {
    resolution: &'a DependencyResolution,
    name: &'a PackageRef,
    format: bool,
}

impl<'a> SourceGenerator<'a> {
    /// Creates a new source generator for the given path to
    /// a binary-encoded target wit package.
    ///
    /// If `format` is true, then `cargo fmt` will be run on the generated source.
    pub fn new(resolution: &'a DependencyResolution, name: &'a PackageRef, format: bool) -> Self {
        Self {
            resolution,
            name,
            format,
        }
    }

    /// Generates the Rust source code for the given world.
    pub async fn generate(&self, world: Option<&str>) -> Result<String> {
        let (resolve, world) = self.decode(world).await?;
        let mut names = ReservedNames::default();
        let generator = ImplementationGenerator::new(&resolve, &resolve.worlds[world], &mut names);

        let mut trie = UseTrie::default();
        trie.reserve_names(&names);

        let impls = generator.generate(&mut trie)?;

        let mut source = String::new();
        writeln!(&mut source, "#[allow(warnings)]\nmod bindings;")?;
        writeln!(&mut source)?;
        write!(
            &mut source,
            "{trie}{nl}",
            nl = if trie.is_empty() { "" } else { "\n" }
        )?;

        writeln!(&mut source, "struct {IMPLEMENTER};\n")?;

        for (i, imp) in impls.iter().enumerate() {
            if i > 0 {
                source.push('\n');
            }

            source.push_str(imp);
        }

        writeln!(
            &mut source,
            "\nbindings::export!({IMPLEMENTER} with_types_in bindings);"
        )?;

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

    async fn decode(&self, world: Option<&str>) -> Result<(Resolve, WorldId)> {
        let (resolve, pkg_id, _) = self.resolution.decode().await?.resolve()?;
        let world = resolve.select_world(pkg_id, world).with_context(|| {
            format!(
                "failed to select world from target package `{name}`",
                name = self.name
            )
        })?;
        Ok((resolve, world))
    }
}
