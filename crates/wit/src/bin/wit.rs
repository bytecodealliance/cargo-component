use anyhow::Result;
use cargo_component_core::{
    registry::CommandError,
    terminal::{Color, Terminal, Verbosity},
};
use clap::Parser;
use dialoguer::{theme::ColorfulTheme, Confirm};
use std::process::exit;
use warg_client::{with_interactive_retry, ClientError, Retry};
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

    with_interactive_retry(|retry: Option<Retry>| async {
        let app = Wit::parse();
        if let Err(e) = match app.command {
            Command::Init(cmd) => cmd.exec(),
            Command::Add(cmd) => cmd.exec(retry).await,
            Command::Build(cmd) => cmd.exec(retry).await,
            Command::Publish(cmd) => cmd.exec(retry).await,
            Command::Key(cmd) => cmd.exec().await,
            Command::Update(cmd) => cmd.exec(retry).await,
        }
        {
            match e {
                CommandError::General(e) => {
                    let terminal = Terminal::new(Verbosity::Normal, Color::Auto);
                    terminal.error(e)?;
                    exit(1);
                }
                CommandError::WargClient(e) => {
                    let terminal = Terminal::new(Verbosity::Normal, Color::Auto);
                    terminal.error(e)?;
                    exit(1);
                }
                CommandError::WargHint(e) => {
                    if let ClientError::PackageDoesNotExistWithHint { name, hint } = e {
                        let hint_reg = hint.to_str().unwrap();
                        let mut terms = hint_reg.split('=');
                        let namespace = terms.next();
                        let registry = terms.next();
                        if let (Some(namespace), Some(registry)) = (namespace, registry) {
                            let prompt = format!(
                          "The package `{}`, does not exist in the registry you're using.\nHowever, the package namespace `{namespace}` does exist in the registry at {registry}.\nWould you like to configure your warg cli to use this registry for packages with this namespace in the future? y/N\n",
                          name.name()
                        );
                            if Confirm::with_theme(&ColorfulTheme::default())
                                .with_prompt(prompt)
                                .interact()
                                .unwrap()
                            {
                                if let Err(e) = match Wit::parse().command {
                                    Command::Init(cmd) => cmd.exec(),
                                    Command::Add(cmd) => {
                                        cmd.exec(Some(Retry::new(
                                            namespace.to_string(),
                                            registry.to_string(),
                                        )))
                                        .await
                                    }
                                    Command::Build(cmd) => {
                                        cmd.exec(Some(Retry::new(
                                            namespace.to_string(),
                                            registry.to_string(),
                                        )))
                                        .await
                                    }
                                    Command::Publish(cmd) => {
                                        cmd.exec(Some(Retry::new(
                                            namespace.to_string(),
                                            registry.to_string(),
                                        )))
                                        .await
                                    }
                                    Command::Key(cmd) => cmd.exec().await,
                                    Command::Update(cmd) => {
                                        cmd.exec(Some(Retry::new(
                                            namespace.to_string(),
                                            registry.to_string(),
                                        )))
                                        .await
                                    }
                                } {
                                    let terminal = Terminal::new(Verbosity::Normal, Color::Auto);
                                    terminal.error(e)?;
                                    exit(1);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }).await?;
    Ok(())
}
