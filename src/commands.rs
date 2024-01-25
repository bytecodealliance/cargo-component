//! Commands for the `cargo-component` CLI.

mod add;
mod key;
mod new;
mod publish;
mod update;

pub use self::add::*;
pub use self::key::*;
pub use self::new::*;
pub use self::publish::*;
pub use self::update::*;
