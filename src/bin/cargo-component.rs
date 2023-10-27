use anyhow::{bail, Result};
use cargo_component::{
    commands::{AddCommand, KeyCommand, NewCommand, PublishCommand, UpdateCommand},
    config::{CargoArguments, Config},
    load_component_metadata, load_metadata, run_cargo_command,
};
use cargo_component_core::terminal::{Color, Terminal, Verbosity};
use clap::{CommandFactory, Parser};

fn version() -> &'static str {
    option_env!("CARGO_VERSION_INFO").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// The list of commands that are built-in to `cargo-component`.
const BUILTIN_COMMANDS: &[&str] = &[
    "add",
    "component", // for indirection via `cargo component`
    "help",
    "init",
    "key",
    "new",
    "publish",
    "remove",
    "rm",
    "update",
    "vendor",
    "yank",
];

/// The list of commands that are explicitly unsupported by `cargo-component`.
///
/// These commands are intended to integrate with `crates.io` and have no
/// analog in `cargo-component` currently.
const UNSUPPORTED_COMMANDS: &[&str] = &[
    "install",
    "login",
    "logout",
    "owner",
    "package",
    "search",
    "uninstall",
];

const AFTER_HELP: &str = "Unrecognized subcommands will be passed to cargo verbatim after\n\
     relevant component bindings are updated.\n\
     \n\
     See `cargo help` for more information on available cargo commands.";

/// Cargo integration for WebAssembly components.
#[derive(Parser)]
#[clap(
    bin_name = "cargo",
    version,
    propagate_version = true,
    arg_required_else_help = true,
    after_help = AFTER_HELP
)]
#[command(version = version())]
enum CargoComponent {
    /// Cargo integration for WebAssembly components.
    #[clap(subcommand, hide = true, after_help = AFTER_HELP)]
    Component(Command), // indirection via `cargo component`
    #[clap(flatten)]
    Command(Command),
}

#[derive(Parser)]
enum Command {
    Add(AddCommand),
    // TODO: Init(InitCommand),
    Key(KeyCommand),
    New(NewCommand),
    // TODO: Remove(RemoveCommand),
    Update(UpdateCommand),
    Publish(PublishCommand),
    // TODO: Yank(YankCommand),
    // TODO: Vendor(VendorCommand),
}

fn detect_subcommand() -> Option<String> {
    let mut iter = std::env::args().skip(1).peekable();

    // Skip the first argument if it is `component` (i.e. `cargo component`)
    if let Some(arg) = iter.peek() {
        if arg == "component" {
            iter.next().unwrap();
        }
    }

    for arg in iter {
        // Break out of processing at the first `--`
        if arg == "--" {
            break;
        }

        if !arg.starts_with('-') {
            return Some(arg);
        }
    }

    None
}

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::init_custom_env("CARGO_COMPONENT_LOG");

    let subcommand = detect_subcommand();
    match subcommand.as_deref() {
        // Check for built-in command or no command (shows help)
        Some(cmd) if BUILTIN_COMMANDS.contains(&cmd) => {
            if let Err(e) = match CargoComponent::parse() {
                CargoComponent::Component(cmd) | CargoComponent::Command(cmd) => match cmd {
                    Command::Add(cmd) => cmd.exec().await,
                    Command::Key(cmd) => cmd.exec().await,
                    Command::New(cmd) => cmd.exec().await,
                    Command::Update(cmd) => cmd.exec().await,
                    Command::Publish(cmd) => cmd.exec().await,
                },
            } {
                let terminal = Terminal::new(Verbosity::Normal, Color::Auto);
                terminal.error(format!("{e:?}"))?;
                std::process::exit(1);
            }
        }

        // Check for explicitly unsupported commands (e.g. those that deal with crates.io)
        Some(cmd) if UNSUPPORTED_COMMANDS.contains(&cmd) => {
            let terminal = Terminal::new(Verbosity::Normal, Color::Auto);
            terminal.error(format!(
                "command `{cmd}` is not supported by `cargo component`\n\n\
                 use `cargo {cmd}` instead"
            ))?;
            std::process::exit(1);
        }

        // If no subcommand was detected,
        None => {
            // Attempt to parse the supported CLI (expected to fail)
            CargoComponent::parse();

            // If somehow the CLI parsed correctly despite no subcommand,
            // print the help instead
            CargoComponent::command().print_long_help()?;
        }

        _ => {
            // Not a built-in command, run the cargo command
            let cargo_args = CargoArguments::parse()?;
            let config = Config::new(Terminal::new(
                if cargo_args.quiet {
                    Verbosity::Quiet
                } else {
                    match cargo_args.verbose {
                        0 => Verbosity::Normal,
                        _ => Verbosity::Verbose,
                    }
                },
                cargo_args.color.unwrap_or_default(),
            ))?;

            let metadata = load_metadata(cargo_args.manifest_path.as_deref())?;
            let packages = load_component_metadata(
                &metadata,
                cargo_args.packages.iter(),
                cargo_args.workspace,
            )?;

            if packages.is_empty() {
                bail!(
                    "manifest `{path}` contains no package or the workspace has no members",
                    path = metadata.workspace_root.join("Cargo.toml")
                );
            }

            let spawn_args: Vec<_> = std::env::args().skip(1).collect();
            if let Err(e) = run_cargo_command(
                &config,
                &metadata,
                &packages,
                subcommand.as_deref(),
                &cargo_args,
                &spawn_args,
            )
            .await
            {
                config.terminal().error(format!("{e:?}"))?;
                std::process::exit(1);
            }
        }
    }

    Ok(())
}
