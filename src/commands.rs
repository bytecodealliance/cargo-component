//! Commands for the `cargo-component` CLI.

mod add;
mod bindings;
mod new;
mod publish;
mod update;

pub use self::add::*;
pub use self::bindings::*;
pub use self::new::*;
pub use self::publish::*;
pub use self::update::*;
