use anyhow::Result;
use cargo::CliError;
use cargo_component::{
    commands::{
        AddCommand, BuildCommand, CheckCommand, ClippyCommand, DocCommand, MetadataCommand,
        NewCommand, PublishCommand, SigningCommand, UpdateCommand,
    },
    config::Config,
};
use clap::Parser;

fn version() -> &'static str {
    option_env!("CARGO_VERSION_INFO").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Cargo integration for WebAssembly components.
#[derive(Parser)]
#[clap(
    bin_name = "cargo",
    version,
    propagate_version = true,
    arg_required_else_help = true
)]
#[command(version = version())]
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
    Doc(DocCommand),
    Metadata(MetadataCommand),
    Check(CheckCommand),
    Add(AddCommand),
    Clippy(ClippyCommand),
    Update(UpdateCommand),
    Publish(PublishCommand),
    Signing(SigningCommand),
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
            Command::Doc(cmd) => cmd.exec(&mut config).await,
            Command::Metadata(cmd) => cmd.exec(&mut config).await,
            Command::Check(cmd) => cmd.exec(&mut config).await,
            Command::Add(cmd) => cmd.exec(&mut config).await,
            Command::Clippy(cmd) => cmd.exec(&mut config).await,
            Command::Update(cmd) => cmd.exec(&mut config).await,
            Command::Publish(cmd) => cmd.exec(&mut config).await,
            Command::Signing(cmd) => cmd.exec(&mut config).await,
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
