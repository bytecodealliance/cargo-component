use anyhow::Result;
use cargo_component_core::terminal::{Color, Terminal, Verbosity};
use clap::Parser;
use std::process::exit;
use wit::commands::{
    AddCommand, BuildCommand, InitCommand, KeyCommand, PublishCommand, UpdateCommand,
};

fn version() -> &'static str {
    option_env!("WIT_VERSION_INFO").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// WIT package tool.
#[derive(Parser)]
#[clap(
    bin_name = "wit",
    version,
    propagate_version = true,
    arg_required_else_help = true
)]
#[command(version = version())]
struct Wit {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Parser)]
pub enum Command {
    Init(InitCommand),
    Add(AddCommand),
    Build(BuildCommand),
    Publish(PublishCommand),
    Key(KeyCommand),
    Update(UpdateCommand),
}

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::init();

    let app = Wit::parse();

    if let Err(e) = match app.command {
        Command::Init(cmd) => cmd.exec().await,
        Command::Add(cmd) => cmd.exec().await,
        Command::Build(cmd) => cmd.exec().await,
        Command::Publish(cmd) => cmd.exec().await,
        Command::Key(cmd) => cmd.exec().await,
        Command::Update(cmd) => cmd.exec().await,
    } {
        let terminal = Terminal::new(Verbosity::Normal, Color::Auto);
        terminal.error(format!("{e:?}"))?;
        exit(1);
    }

    Ok(())
}
