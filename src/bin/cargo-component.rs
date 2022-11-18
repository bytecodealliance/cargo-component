use anyhow::Result;
use cargo::CliError;
use cargo_component::{
    commands::{
        AddCommand, BuildCommand, CheckCommand, ClippyCommand, MetadataCommand, NewCommand,
        RegistryCommand,
    },
    config::Config,
};
use clap::Parser;

/// Cargo integration for WebAssembly components.
#[derive(Parser)]
#[clap(
    bin_name = "cargo",
    version,
    propagate_version = true,
    arg_required_else_help = true
)]
enum CargoComponent {
    /// Cargo integration for WebAssembly components.
    #[clap(subcommand, hide = true)]
    Component(Command), // indirection via `cargo component`
    #[clap(flatten)]
    Command(Command),
}

#[derive(Parser)]
pub enum Command {
    New(NewCommand),
    Build(BuildCommand),
    Metadata(MetadataCommand),
    Check(CheckCommand),
    Add(AddCommand),
    Clippy(ClippyCommand),
    Registry(RegistryCommand),
}

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(feature = "pretty_env_logger")]
    pretty_env_logger::init_custom_env("CARGO_COMPONENT_LOG");

    let mut config = Config::default()?;

    if let Err(e) = match CargoComponent::parse() {
        CargoComponent::Component(cmd) | CargoComponent::Command(cmd) => match cmd {
            Command::New(cmd) => cmd.exec(&mut config).await,
            Command::Build(cmd) => cmd.exec(&mut config).await,
            Command::Metadata(cmd) => cmd.exec(&mut config).await,
            Command::Check(cmd) => cmd.exec(&mut config).await,
            Command::Add(cmd) => cmd.exec(&mut config).await,
            Command::Clippy(cmd) => cmd.exec(&mut config).await,
            Command::Registry(cmd) => cmd.exec(&mut config).await,
        },
    } {
        cargo::exit_with_error(
            CliError {
                error: Some(e),
                exit_code: 1,
            },
            &mut config.shell(),
        );
    }

    Ok(())
}
