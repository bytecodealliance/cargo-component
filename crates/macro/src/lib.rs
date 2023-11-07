//! A proc-macro crate for generating bindings with `cargo-component`.

#![deny(missing_docs)]

use proc_macro2::{Span, TokenStream};
use quote::quote;
use std::path::{Path, PathBuf};
use syn::{Error, Result};

fn bindings_source_path() -> Result<PathBuf> {
    let path = Path::new(env!("CARGO_TARGET_DIR"))
        .join("bindings")
        .join(
            std::env::var("CARGO_PKG_NAME")
                .expect("failed to get `CARGO_PKG_NAME` environment variable"),
        )
        .join("bindings.rs");

    if !path.is_file() {
        return Err(Error::new(
            Span::call_site(),
            format!(
                "bindings file `{path}` does not exist\n\n\
                 did you forget to run `cargo component build`? (https://github.com/bytecodealliance/cargo-component)",
                path = path.display()
            ),
        ));
    }

    Ok(path)
}

fn generate_bindings(input: proc_macro::TokenStream) -> Result<TokenStream> {
    if !input.is_empty() {
        return Err(Error::new(
            Span::call_site(),
            "the `generate!` macro does not take any arguments",
        ));
    }

    let path = bindings_source_path()?;
    let path = path.to_str().expect("bindings path is not valid UTF-8");

    Ok(quote! {
        /// Generated bindings module for this component.
        #[allow(dead_code)]
        pub(crate) mod bindings {
            include!(#path);
        }
    })
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
/// # Settings
///
/// Use the `package.metadata.component.bindings` section in
/// `Cargo.toml` to configure bindings generation.
///
/// The available settings are:
///
/// - `implementor`: The name of the type to implement world exports on.
/// - `resources`: A map of resource names to resource implementor types.
/// - `ownership`: The ownership model to use for resources.
/// - `derives`: Additional derive macro attributes to add to generated types.
///
/// # Examples
///
/// Specifying a custom implementor type named `MyComponent`:
///
/// ```toml
/// [package.metadata.component.bindings]
/// implementor = "MyComponent"
/// ```
///
/// Specifying a custom resource implementor type named `MyResource`:
///
/// ```toml
/// [package.metadata.component.bindings.resources]
/// "my:package/iface/res" = "MyResource"
/// ```
///
/// Specifying the `borrowing-duplicate-if-necessary` ownership model:
///
/// ```toml
/// [package.metadata.component.bindings]
/// ownership = "borrowing-duplicate-if-necessary"
/// ````
#[proc_macro]
pub fn generate(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    generate_bindings(input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}
