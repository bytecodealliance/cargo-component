//! A module for implementing the Rust source generator used for
//! the `--target` option of the `new` command.

use anyhow::{bail, Context, Result};
use heck::{AsSnakeCase, ToSnakeCase, ToUpperCamelCase};
use indexmap::IndexSet;
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt::{self, Write},
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
};
use warg_protocol::registry::PackageId;
use wit_bindgen_rust_lib::to_rust_ident;
use wit_component::DecodedWasm;
use wit_parser::{
    Function, Interface, Resolve, Type, TypeDef, TypeDefKind, TypeId, TypeOwner, WorldId,
    WorldItem, WorldKey,
};

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
            WorldKey::Name(name) => self.insert(["bindings", "exports", name.as_str()], name),
            WorldKey::Interface(id) => {
                let iface = &resolve.interfaces[*id];
                self.insert_interface_type(
                    resolve,
                    iface,
                    iface.name.as_deref().expect("unnamed interface"),
                )
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

/// Represents a Rust source code generator for targeting a given wit package.
///
/// The generated source defines a component that will implement the expected
/// export traits for the given world.
pub struct SourceGenerator<'a> {
    id: &'a PackageId,
    path: &'a Path,
    format: bool,
}

impl<'a> SourceGenerator<'a> {
    /// Creates a new source generator for the given path to
    /// a binary-encoded target wit package.
    ///
    /// If `format` is true, then `cargo fmt` will be run on the generated source.
    pub fn new(id: &'a PackageId, path: &'a Path, format: bool) -> Self {
        Self { id, path, format }
    }

    /// Generates the Rust source code for the given world.
    pub fn generate(&self, world: Option<&str>) -> Result<String> {
        let (resolve, world) = self.decode(world)?;
        let mut trie = UseTrie::default();
        let mut impls = Vec::new();
        let world = &resolve.worlds[world];

        let mut function_exports = Vec::new();
        for (key, item) in &world.exports {
            match item {
                WorldItem::Function(f) => {
                    function_exports.push(f);
                }
                WorldItem::Interface(i) => {
                    let interface = &resolve.interfaces[*i];
                    let mut imp: String = String::new();
                    writeln!(
                        &mut imp,
                        "\nimpl {name} for Component {{",
                        name = trie.insert_export_trait(&resolve, key),
                    )
                    .unwrap();

                    for (i, (_, func)) in interface.functions.iter().enumerate() {
                        if i > 0 {
                            imp.push('\n');
                        }
                        Self::print_unimplemented_func(&resolve, func, &mut imp, &mut trie)?;
                    }

                    imp.push_str("}\n");
                    impls.push(imp);
                }
                WorldItem::Type(_) => continue,
            }
        }

        if !function_exports.is_empty() {
            let mut imp = String::new();

            writeln!(
                &mut imp,
                "\nimpl {name} for Component {{",
                name = trie.insert(["bindings"], &world.name)
            )
            .unwrap();

            for (i, func) in function_exports.iter().enumerate() {
                if i > 0 {
                    imp.push('\n');
                }
                Self::print_unimplemented_func(&resolve, func, &mut imp, &mut trie)?;
            }

            imp.push_str("}\n");
            impls.push(imp);
        }

        let mut source = String::new();
        writeln!(&mut source, "// Required for component bindings generation")?;
        writeln!(&mut source, "cargo_component_bindings::generate!();")?;
        writeln!(&mut source)?;
        write!(
            &mut source,
            "{trie}{nl}",
            nl = if trie.is_empty() { "" } else { "\n" }
        )?;

        source.push_str("struct Component;\n");

        for (i, imp) in impls.iter().enumerate() {
            if i > 0 {
                source.push_str("\n\n");
            }

            source.push_str(imp);
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
        trie: &mut UseTrie,
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
            Self::print_type(resolve, param, source, trie)?;
        }
        source.push(')');
        match func.results.len() {
            0 => {}
            1 => {
                source.push_str(" -> ");
                Self::print_type(
                    resolve,
                    func.results.iter_types().next().unwrap(),
                    source,
                    trie,
                )?;
            }
            _ => {
                source.push_str(" -> (");
                for (i, ty) in func.results.iter_types().enumerate() {
                    if i > 0 {
                        source.push_str(", ");
                    }
                    Self::print_type(resolve, ty, source, trie)?;
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
        source: &mut String,
        trie: &mut UseTrie,
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
            Type::Id(id) => Self::print_type_id(resolve, *id, source, trie)?,
        }

        Ok(())
    }

    fn print_type_id(
        resolve: &Resolve,
        id: TypeId,
        source: &mut String,
        trie: &mut UseTrie,
    ) -> Result<()> {
        let ty = &resolve.types[id];

        if ty.name.is_some() {
            Self::print_type_path(resolve, ty, source, trie);
            return Ok(());
        }

        match &ty.kind {
            TypeDefKind::List(ty) => {
                source.push_str("Vec<");
                Self::print_type(resolve, ty, source, trie)?;
                source.push('>');
            }
            TypeDefKind::Option(ty) => {
                source.push_str("Option<");
                Self::print_type(resolve, ty, source, trie)?;
                source.push('>');
            }
            TypeDefKind::Result(r) => {
                source.push_str("Result<");
                Self::print_optional_type(resolve, r.ok.as_ref(), source, trie)?;
                source.push_str(", ");
                Self::print_optional_type(resolve, r.err.as_ref(), source, trie)?;
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
                    Self::print_type(resolve, ty, source, trie)?;
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
                Self::print_optional_type(resolve, ty.as_ref(), source, trie)?;
                source.push('>');
            }
            TypeDefKind::Stream(stream) => {
                source.push_str("Stream<");
                Self::print_optional_type(resolve, stream.element.as_ref(), source, trie)?;
                source.push_str(", ");
                Self::print_optional_type(resolve, stream.end.as_ref(), source, trie)?;
                source.push('>');
            }
            TypeDefKind::Type(ty) => Self::print_type(resolve, ty, source, trie)?,
            TypeDefKind::Resource | TypeDefKind::Handle(_) => {
                todo!("implement resources support")
            }
            TypeDefKind::Unknown => unreachable!(),
        }

        Ok(())
    }

    fn print_type_path(resolve: &Resolve, ty: &TypeDef, source: &mut String, trie: &mut UseTrie) {
        if let TypeOwner::Interface(id) = ty.owner {
            let interface = &resolve.interfaces[id];
            if interface.package.is_some() {
                write!(
                    source,
                    "{name}",
                    name =
                        trie.insert_interface_type(resolve, interface, ty.name.as_deref().unwrap())
                )
                .unwrap();
                return;
            }
        }

        write!(
            source,
            "{name}",
            name = trie.insert(["bindings"], ty.name.as_deref().unwrap())
        )
        .unwrap();
    }

    fn print_optional_type(
        resolve: &Resolve,
        ty: Option<&Type>,
        source: &mut String,
        trie: &mut UseTrie,
    ) -> Result<()> {
        match ty {
            Some(ty) => Self::print_type(resolve, ty, source, trie)?,
            None => source.push_str("()"),
        }

        Ok(())
    }
}
