//! A proc-macro crate for generating bindings with `cargo-component`.

#![deny(missing_docs)]

use heck::ToUpperCamelCase;
use proc_macro2::{Span, TokenStream};
use quote::quote;
use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
};
use syn::{
    parse::{Parse, ParseStream},
    parse_quote,
    punctuated::Punctuated,
    token, Error, Result, Token,
};
use wit_bindgen_core::{
    wit_parser::{Resolve, TypeDefKind, WorldId, WorldItem, WorldKey},
    Files,
};
use wit_bindgen_rust::{ExportKey, Opts};
use wit_bindgen_rust_lib::Ownership;
use wit_component::DecodedWasm;

fn implementor_path_str(path: &syn::Path) -> String {
    let mut s = String::new();
    s.push_str("super::");

    for (i, segment) in path.segments.iter().enumerate() {
        if i > 0 {
            s.push_str("::");
        }

        write!(&mut s, "{ident}", ident = segment.ident).unwrap();
    }

    s
}

/// Used to generate bindings for a WebAssembly component.
///
/// By default, all world exports are expected to be implemented
/// on a type named `Component` where the `bindings!` macro
/// is invoked.
///
/// Additionally, all resource exports are expected to be
/// implemented on a type named `<ResourceName>`.
///
/// For example, a resource named `file` would be implemented
/// on a type named `File` in the same scope as the `generate!`
/// macro invocation.
///
/// # Options
///
/// The macro accepts the following options:
///
/// - `implementor`: The name of the type to implement world exports on.
/// - `resources`: A map of resource names to resource implementor types.
/// - `ownership`: The ownership model to use for resources.
///
/// # Examples
///
/// Using the default implementor names:
///
/// ```ignore
/// cargo_component_bindings::generate!()
/// ```
///
/// Specifying a custom implementor type named `MyComponent`:
///
/// ```ignore
/// cargo_component_bindings::generate!({
///     implementor: MyComponent,
/// })
/// ```
///
/// Specifying a custom resource implementor type named `MyResource`:
///
/// ```ignore
/// cargo_component_bindings::generate!({
///     resources: {
///         "my:package/iface/res": MyResource,
///     }
/// })
/// ```
///
/// Specifying the `borrowing-duplicate-if-necessary` ownership model
/// for resources:
///
/// ```ignore
/// cargo_component_bindings::generate!({
///      ownership: borrowing-duplicate-if-necessary
/// })
#[proc_macro]
pub fn generate(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    syn::parse_macro_input!(input as Config)
        .expand()
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn target_path() -> Result<PathBuf> {
    Ok(Path::new(env!("CARGO_TARGET_DIR"))
        .join("bindings")
        .join(
            std::env::var("CARGO_PKG_NAME")
                .expect("failed to get `CARGO_PKG_NAME` environment variable"),
        )
        .join("target.wasm"))
}

fn decode_resolve(path: &Path, span: Span) -> Result<(Resolve, WorldId)> {
    let bytes = std::fs::read(path).map_err(|e| {
        Error::new(
            span,
            format!(
                "failed to read target file `{path}`: {e}\n\n\
                 did you forget to run `cargo component build`? (https://github.com/bytecodealliance/cargo-component)",
                path = path.display()
            ),
        )
    })?;

    let decoded = wit_component::decode(&bytes).map_err(|e| {
        Error::new(
            span,
            format!(
                "failed to decode target file `{path}`: {e}",
                path = path.display()
            ),
        )
    })?;

    let world_path = path.with_file_name("world");
    let world = fs::read_to_string(&world_path).map_err(|e| {
        Error::new(
            span,
            format!(
                "failed to read world file `{path}`: {e}",
                path = world_path.display()
            ),
        )
    })?;

    match decoded {
        DecodedWasm::WitPackage(resolve, pkg) => {
            let world = resolve
                .select_world(pkg, if world.is_empty() { None } else { Some(&world) })
                .map_err(|e| Error::new(span, format!("failed to select world for target: {e}")))?;
            Ok((resolve, world))
        }
        DecodedWasm::Component(_, _) => Err(Error::new(
            span,
            format!(
                "target file `{path}` is not a WIT package",
                path = path.display()
            ),
        )),
    }
}

mod kw {
    syn::custom_keyword!(implementor);
    syn::custom_keyword!(resources);
    syn::custom_keyword!(ownership);
}

#[derive(Clone)]
struct Resource {
    key: syn::LitStr,
    value: syn::Path,
}

impl Parse for Resource {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let key = input.parse()?;
        input.parse::<Token![:]>()?;
        let value = input.parse()?;
        Ok(Self { key, value })
    }
}

enum Opt {
    Implementor(Span, syn::Path),
    Resources(Span, Vec<Resource>),
    Ownership(Span, Ownership),
}

impl Parse for Opt {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let l = input.lookahead1();
        if l.peek(kw::implementor) {
            let span = input.parse::<kw::implementor>()?.span;
            input.parse::<Token![:]>()?;
            Ok(Opt::Implementor(span, input.parse()?))
        } else if l.peek(kw::resources) {
            let span = input.parse::<kw::resources>()?.span;
            input.parse::<Token![:]>()?;
            let contents;
            syn::braced!(contents in input);
            Ok(Opt::Resources(
                span,
                Punctuated::<_, Token![,]>::parse_terminated(&contents)?
                    .iter()
                    .cloned()
                    .collect(),
            ))
        } else if l.peek(kw::ownership) {
            let span = input.parse::<kw::ownership>()?.span;
            input.parse::<Token![:]>()?;
            let ownership = input.parse::<syn::Ident>()?;
            Ok(Opt::Ownership(
                span,
                ownership
                    .to_string()
                    .parse()
                    .map_err(|e| Error::new(ownership.span(), e))?,
            ))
        } else {
            Err(l.error())
        }
    }
}

struct Config {
    input: PathBuf,
    resolve: Resolve,
    world: WorldId,
    implementor: Option<syn::Path>,
    resources: HashMap<String, syn::Path>,
    ownership: Ownership,
}

impl Config {
    fn expand(self) -> Result<TokenStream> {
        fn resource_implementor(
            key: &str,
            name: &str,
            resources: &HashMap<String, syn::Path>,
        ) -> String {
            implementor_path_str(&resources.get(key).map(Cow::Borrowed).unwrap_or_else(|| {
                Cow::Owned(
                    syn::PathSegment::from(syn::Ident::new(
                        &name.to_upper_camel_case(),
                        Span::call_site(),
                    ))
                    .into(),
                )
            }))
        }

        let implementor =
            implementor_path_str(&self.implementor.unwrap_or_else(|| parse_quote!(Component)));

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
                                let implementor = resource_implementor(&key, name, &self.resources);
                                exports.insert(ExportKey::Name(key), implementor);
                            }
                            _ => continue,
                        }
                    }

                    implementor.clone()
                }
                WorldItem::Type(id) => match self.resolve.types[*id].kind {
                    TypeDefKind::Resource => resource_implementor(&key, &key, &self.resources),
                    _ => continue,
                },
                WorldItem::Function(_) => implementor.clone(),
            };

            exports.insert(ExportKey::Name(key), implementor);
        }

        let opts = Opts {
            exports,
            ownership: self.ownership,
            runtime_path: Some("cargo_component_bindings::rt".to_string()),
            ..Default::default()
        };

        let mut files = Files::default();
        opts.build()
            .generate(&self.resolve, self.world, &mut files)
            .map_err(|e| {
                Error::new(
                    Span::call_site(),
                    format!(
                        "failed to generate bindings from `{path}`: {e}",
                        path = self.input.display()
                    ),
                )
            })?;

        let sources: Vec<_> = files
            .iter()
            .map(|(_, s)| std::str::from_utf8(s).unwrap())
            .collect();
        assert!(
            sources.len() == 1,
            "expected exactly one source file to be generated"
        );

        let source = sources[0].parse::<TokenStream>()?;
        let input = self.input.display().to_string();

        Ok(quote! {
            pub(crate) mod bindings {
                #source

                const _: &[u8] = include_bytes!(#input);
            }
        })
    }
}

impl Parse for Config {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let mut implementor: Option<syn::Path> = None;
        let mut resources: Option<Vec<Resource>> = None;
        let mut ownership: Option<Ownership> = None;

        if input.peek(token::Brace) {
            let content;
            syn::braced!(content in input);
            let options = Punctuated::<Opt, Token![,]>::parse_terminated(&content)?;
            for option in options.into_pairs() {
                match option.into_value() {
                    Opt::Implementor(span, value) => {
                        if implementor.is_some() {
                            return Err(Error::new(
                                span,
                                "cannot specify `implementor` more than once",
                            ));
                        }

                        if let Some(segment) = value.segments.first() {
                            if segment.ident == "self" || segment.ident == "crate" {
                                return Err(Error::new(
                                    segment.ident.span(),
                                    "cannot use `self` or `crate` as the implementor path",
                                ));
                            }
                        }

                        implementor = Some(value);
                    }
                    Opt::Resources(span, value) => {
                        if resources.is_some() {
                            return Err(Error::new(
                                span,
                                "cannot specify `resources` more than once",
                            ));
                        }

                        resources = Some(value);
                    }
                    Opt::Ownership(span, value) => {
                        if ownership.is_some() {
                            return Err(Error::new(
                                span,
                                "cannot specify `ownership` more than once",
                            ));
                        }

                        ownership = Some(value);
                    }
                }
            }
        }

        let input = target_path()?;
        let (resolve, world) = decode_resolve(&input, Span::call_site())?;

        Ok(Config {
            input,
            resolve,
            world,
            implementor,
            resources: resources
                .map(|r| r.into_iter().map(|r| (r.key.value(), r.value)).collect())
                .unwrap_or_default(),
            ownership: ownership.unwrap_or_default(),
        })
    }
}
