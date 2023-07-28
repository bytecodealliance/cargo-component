//! A crate for generating bindings with `cargo-component`.

#![deny(missing_docs)]

// Export the `generate` macro.
pub use cargo_component_macro::generate;

// Re-export `wit_bindgen::rt` module for the generated code to use.
#[doc(hidden)]
pub use wit_bindgen::rt;
