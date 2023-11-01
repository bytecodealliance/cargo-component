//! Commands for the `cargo-component` CLI.

mod add;
mod key;
mod new;
mod publish;
mod update;
mod upgrade;

pub use self::add::*;
pub use self::key::*;
pub use self::new::*;
pub use self::publish::*;
pub use self::update::*;
pub use self::upgrade::*;
