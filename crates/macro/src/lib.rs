//! A proc-macro crate for generating bindings with `cargo-component`.

#![deny(missing_docs)]

use proc_macro2::{Span, TokenStream};
use quote::quote;
use std::{
    fs,
    path::{Path, PathBuf},
};
use syn::{
    parse::{Parse, ParseStream},
    parse_quote,
    punctuated::Punctuated,
    spanned::Spanned,
    token, Error, Result, Token, TypePath,
};
use wit_bindgen_core::{
    wit_parser::{Resolve, WorldId},
    Files,
};
use wit_bindgen_rust::Opts;
use wit_component::DecodedWasm;

/// Used to generate bindings for a WebAssembly component.
///
/// # Examples
///
/// Using the default implementor of `Component`:
///
/// ```ignore
/// cargo_component_bindings::generate!()
/// ```
///
/// Specifying a custom implementor type named `MyComponent`:
///
/// ```ignore
/// cargo_component_bindings::generate!({
///    implementor: MyComponent,
/// })
/// ```
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
}

enum Opt {
    Implementor(TypePath),
}

impl Parse for Opt {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let l = input.lookahead1();
        if l.peek(kw::implementor) {
            input.parse::<kw::implementor>()?;
            input.parse::<Token![:]>()?;
            Ok(Opt::Implementor(input.parse()?))
        } else {
            Err(l.error())
        }
    }
}

struct Config {
    input: PathBuf,
    resolve: Resolve,
    world: WorldId,
    implementor: Option<TypePath>,
}

impl Config {
    fn expand(self) -> Result<TokenStream> {
        let implementor = self.implementor.unwrap_or_else(|| parse_quote!(Component));

        let opts = Opts {
            macro_call_prefix: Some("crate::bindings::".to_string()),
            export_macro_name: Some("export".to_string()),
            runtime_path: Some("cargo_component_bindings::rt".to_string()),
            ..Default::default()
        };

        let mut files = Files::default();
        opts.build().generate(&self.resolve, self.world, &mut files);

        let sources: Vec<_> = files
            .iter()
            .map(|(_, s)| std::str::from_utf8(s).unwrap())
            .collect();
        assert!(
            sources.len() == 1,
            "expected exactly one source file to be generated"
        );

        let (use_export, export) = if self.resolve.worlds[self.world].exports.is_empty() {
            (quote!(), quote!())
        } else {
            (
                quote!(
                    pub(crate) use export;
                ),
                quote!(crate::bindings::export!(#implementor);),
            )
        };

        let source = sources[0].parse::<TokenStream>()?;
        let input = self.input.display().to_string();

        Ok(quote! {
            mod bindings {
                #source

                #use_export

                const _: &[u8] = include_bytes!(#input);
            }

            #export
        })
    }
}

impl Parse for Config {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        let call_site = Span::call_site();
        let mut implementor = None;

        if input.peek(token::Brace) {
            let content;
            syn::braced!(content in input);
            let options = Punctuated::<Opt, Token![,]>::parse_terminated(&content)?;
            for option in options.into_pairs() {
                match option.into_value() {
                    Opt::Implementor(path) => {
                        if implementor.is_some() {
                            return Err(Error::new(
                                path.span(),
                                "cannot specify `implementor` more than once",
                            ));
                        }

                        implementor = Some(path);
                    }
                }
            }
        }

        let input = target_path()?;
        let (resolve, world) = decode_resolve(&input, call_site)?;

        Ok(Config {
            input,
            resolve,
            world,
            implementor,
        })
    }
}
