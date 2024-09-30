//! Module for cargo-component configuration.
//!
//! This implements an argument parser because `clap` is not
//! designed for parsing unknown or unsupported arguments.
//!
//! See https://github.com/clap-rs/clap/issues/1404 for some
//! discussion around this issue.
//!
//! To properly "wrap" `cargo` commands, we need to be able to
//! detect certain arguments, but not error out if the arguments
//! are otherwise unknown as they will be passed to `cargo` directly.
//!
//! This will allow `cargo-component` to be used as a drop-in
//! replacement for `cargo` without having to be fully aware of
//! the many subcommands and options that `cargo` supports.
//!
//! What is detected here is the minimal subset of the arguments
//! that `cargo` supports which are necessary for `cargo-component`
//! to function.

use anyhow::{anyhow, bail, Context, Result};
use cargo_component_core::cache_dir;
use cargo_component_core::terminal::{Color, Terminal};
use cargo_metadata::Metadata;
use parse_arg::{iter_short, match_arg};
use semver::Version;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::{collections::BTreeMap, fmt::Display, path::PathBuf};
use toml_edit::DocumentMut;
use wasm_pkg_client::caching::{CachingClient, FileCache};
use wasm_pkg_client::Client;

/// Represents a cargo package specifier.
///
/// See `cargo help pkgid` for more information.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CargoPackageSpec {
    /// The name of the package, e.g. `foo`.
    pub name: String,
    /// The version of the package, if specified.
    pub version: Option<Version>,
}

impl CargoPackageSpec {
    /// Creates a new package specifier from a string.
    pub fn new(spec: impl Into<String>) -> Result<Self> {
        let spec = spec.into();

        // Bail out if the package specifier contains a URL.
        if spec.contains("://") {
            bail!("URL package specifier `{spec}` is not supported");
        }

        Ok(match spec.split_once('@') {
            Some((name, version)) => Self {
                name: name.to_string(),
                version: Some(
                    version
                        .parse()
                        .with_context(|| format!("invalid package specified `{spec}`"))?,
                ),
            },
            None => Self {
                name: spec,
                version: None,
            },
        })
    }

    /// Loads Cargo.toml in the current directory, attempts to find the matching package from metadata.
    pub fn find_current_package_spec(metadata: &Metadata) -> Option<Self> {
        let current_manifest = std::fs::read_to_string("Cargo.toml").ok()?;
        let document: DocumentMut = current_manifest.parse().ok()?;
        let name = document["package"]["name"].as_str()?;
        let version = metadata
            .packages
            .iter()
            .find(|found| found.name == name)
            .map(|found| found.version.clone());
        Some(CargoPackageSpec {
            name: name.to_string(),
            version,
        })
    }
}

impl FromStr for CargoPackageSpec {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::new(s)
    }
}

impl fmt::Display for CargoPackageSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{name}", name = self.name)?;
        if let Some(version) = &self.version {
            write!(f, "@{version}")?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
enum Arg {
    Flag {
        name: &'static str,
        short: Option<char>,
        value: bool,
    },
    Single {
        name: &'static str,
        value_name: &'static str,
        short: Option<char>,
        value: Option<String>,
    },
    Multiple {
        name: &'static str,
        value_name: &'static str,
        short: Option<char>,
        values: Vec<String>,
    },
    Counting {
        name: &'static str,
        short: Option<char>,
        value: usize,
    },
}

impl Arg {
    fn name(&self) -> &'static str {
        match self {
            Self::Flag { name, .. }
            | Self::Single { name, .. }
            | Self::Multiple { name, .. }
            | Self::Counting { name, .. } => name,
        }
    }

    fn short(&self) -> Option<char> {
        match self {
            Self::Flag { short, .. }
            | Self::Single { short, .. }
            | Self::Multiple { short, .. }
            | Self::Counting { short, .. } => *short,
        }
    }

    fn expects_value(&self) -> bool {
        matches!(self, Self::Single { .. } | Self::Multiple { .. })
    }

    fn set_value(&mut self, v: String) -> Result<()> {
        match self {
            Self::Single { value, .. } => {
                if value.is_some() {
                    bail!("the argument '{self}' cannot be used multiple times");
                }

                *value = Some(v);
                Ok(())
            }
            Self::Multiple { values, .. } => {
                values.push(v);
                Ok(())
            }
            _ => unreachable!(),
        }
    }

    fn set_present(&mut self) -> Result<()> {
        match self {
            Self::Flag { value, .. } => {
                if *value {
                    bail!("the argument '{self}' cannot be used multiple times");
                }

                *value = true;
                Ok(())
            }
            Self::Counting { value, .. } => {
                *value += 1;
                Ok(())
            }
            _ => unreachable!(),
        }
    }

    fn take_single(&mut self) -> Option<String> {
        match self {
            Self::Single { value, .. } => value.take(),
            _ => None,
        }
    }

    fn take_multiple(&mut self) -> Vec<String> {
        match self {
            Self::Multiple { values, .. } => std::mem::take(values),
            _ => Vec::new(),
        }
    }

    fn count(&self) -> usize {
        match self {
            Arg::Flag { value, .. } => *value as usize,
            Arg::Single { value, .. } => value.is_some() as usize,
            Arg::Multiple { values, .. } => values.len(),
            Arg::Counting { value, .. } => *value,
        }
    }

    #[cfg(test)]
    fn reset(&mut self) {
        match self {
            Arg::Flag { value, .. } => *value = false,
            Arg::Single { value, .. } => *value = None,
            Arg::Multiple { values, .. } => values.clear(),
            Arg::Counting { value, .. } => *value = 0,
        }
    }
}

impl Display for Arg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{name}", name = self.name())?;
        match self {
            Self::Single { value_name, .. } | Self::Multiple { value_name, .. } => {
                write!(f, " <{value_name}>")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Default, Debug, Clone)]
struct Args {
    args: Vec<Arg>,
    long: BTreeMap<&'static str, usize>,
    short: BTreeMap<char, usize>,
}

impl Args {
    fn flag(self, name: &'static str, short: Option<char>) -> Self {
        self.insert(Arg::Flag {
            name,
            short,
            value: false,
        })
    }

    fn single(self, name: &'static str, value_name: &'static str, short: Option<char>) -> Self {
        self.insert(Arg::Single {
            name,
            value_name,
            short,
            value: None,
        })
    }

    fn multiple(self, name: &'static str, value_name: &'static str, short: Option<char>) -> Self {
        self.insert(Arg::Multiple {
            name,
            value_name,
            short,
            values: Vec::new(),
        })
    }

    fn counting(self, name: &'static str, short: Option<char>) -> Self {
        self.insert(Arg::Counting {
            name,
            short,
            value: 0,
        })
    }

    fn get(&mut self, name: &str) -> Option<&Arg> {
        self.long.get(name).copied().map(|i| &self.args[i])
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut Arg> {
        self.long.get(name).copied().map(|i| &mut self.args[i])
    }

    fn get_short_mut(&mut self, short: char) -> Option<&mut Arg> {
        self.short.get(&short).copied().map(|i| &mut self.args[i])
    }

    fn insert(mut self, arg: Arg) -> Self {
        let name = arg.name();
        let short = arg.short();

        let index = self.args.len();
        self.args.push(arg);

        let prev = self.long.insert(name, index);
        assert!(prev.is_none(), "duplicate argument `{name}` provided");

        if let Some(short) = short {
            let prev = self.short.insert(short, index);
            assert!(prev.is_none(), "duplicate argument `-{short}` provided");
        }

        self
    }

    /// Parses an argument as an option.
    ///
    /// Returns `Ok(true)` if the argument is an option.
    ///
    /// Returns `Ok(false)` if the argument is not an option.
    fn parse(&mut self, arg: &str, iter: &mut impl Iterator<Item = String>) -> Result<bool> {
        // Handle short options
        if let Some(mut short) = iter_short(arg) {
            while let Some(c) = short.next() {
                if let Some(option) = self.get_short_mut(c) {
                    if option.expects_value() {
                        let value: String = short.parse_remaining(iter).map_err(|_| {
                            anyhow!("a value is required for '{option}' but none was supplied")
                        })?;

                        // Strip a leading `=` out of the value if present
                        option
                            .set_value(value.strip_prefix('=').map(Into::into).unwrap_or(value))?;
                        return Ok(true);
                    }

                    option.set_present()?;
                }
            }

            // The argument is an option
            return Ok(true);
        }

        // Handle long options
        if arg.starts_with("--") {
            if let Some(option) = self.get_mut(arg.split_once('=').map(|(n, _)| n).unwrap_or(arg)) {
                if option.expects_value() {
                    if let Some(v) = match_arg(option.name(), &arg, iter) {
                        option.set_value(v.map_err(|_| {
                            anyhow!("a value is required for '{option}' but none was supplied")
                        })?)?;
                    }
                } else if option.name() == arg {
                    option.set_present()?;
                }
            }

            // The argument is an option
            return Ok(true);
        }

        // Not an option
        Ok(false)
    }
}

/// Represents known cargo arguments.
///
/// This is a subset of the arguments that cargo supports that
/// are necessary for cargo-component to function.
#[derive(Default, Debug, Clone, Eq, PartialEq)]
pub struct CargoArguments {
    /// The --color argument.
    pub color: Option<Color>,
    /// The (count of) --verbose argument.
    pub verbose: usize,
    /// The --help argument.
    pub help: bool,
    /// The --quiet argument.
    pub quiet: bool,
    /// The --target argument.
    pub targets: Vec<String>,
    /// The --manifest-path argument.
    pub manifest_path: Option<PathBuf>,
    /// The `--message-format`` argument.
    pub message_format: Option<String>,
    /// The --frozen argument.
    pub frozen: bool,
    /// The --locked argument.
    pub locked: bool,
    /// The --release argument.
    pub release: bool,
    /// The --offline argument.
    pub offline: bool,
    /// The --workspace argument.
    pub workspace: bool,
    /// The --package argument.
    pub packages: Vec<CargoPackageSpec>,
}

impl CargoArguments {
    /// Determines if network access is allowed based on the configuration.
    pub fn network_allowed(&self) -> bool {
        !self.frozen && !self.offline
    }

    /// Determines if an update to the lock file is allowed based on the configuration.
    pub fn lock_update_allowed(&self) -> bool {
        !self.frozen && !self.locked
    }

    /// Parses the arguments from the environment.
    pub fn parse() -> Result<Self> {
        Self::parse_from(std::env::args().skip(1))
    }

    /// Parses the arguments from an iterator.
    fn parse_from<T>(iter: impl Iterator<Item = T>) -> Result<Self>
    where
        T: Into<String>,
    {
        let mut args = Args::default()
            .single("--color", "WHEN", Some('c'))
            .single("--manifest-path", "PATH", None)
            .single("--message-format", "FMT", None)
            .multiple("--package", "SPEC", Some('p'))
            .multiple("--target", "TRIPLE", None)
            .flag("--release", Some('r'))
            .flag("--frozen", None)
            .flag("--locked", None)
            .flag("--offline", None)
            .flag("--all", None)
            .flag("--workspace", None)
            .counting("--verbose", Some('v'))
            .flag("--quiet", Some('q'))
            .flag("--help", Some('h'));

        let mut iter = iter.map(Into::into).peekable();

        // Skip the first argument if it is `component`
        if let Some(arg) = iter.peek() {
            if arg == "component" {
                iter.next().unwrap();
            }
        }

        while let Some(arg) = iter.next() {
            // Break out of processing at the first `--`
            if arg == "--" {
                break;
            }

            // Parse options
            if args.parse(&arg, &mut iter)? {
                continue;
            }
        }

        Ok(Self {
            color: args
                .get_mut("--color")
                .unwrap()
                .take_single()
                .map(|v| v.parse())
                .transpose()?,
            verbose: args.get("--verbose").unwrap().count(),
            help: args.get("--help").unwrap().count() > 0,
            quiet: args.get("--quiet").unwrap().count() > 0,
            manifest_path: args
                .get_mut("--manifest-path")
                .unwrap()
                .take_single()
                .map(PathBuf::from),
            message_format: args.get_mut("--message-format").unwrap().take_single(),
            targets: args.get_mut("--target").unwrap().take_multiple(),
            frozen: args.get("--frozen").unwrap().count() > 0,
            locked: args.get("--locked").unwrap().count() > 0,
            offline: args.get("--offline").unwrap().count() > 0,
            release: args.get("--release").unwrap().count() > 0,
            workspace: args.get("--workspace").unwrap().count() > 0
                || args.get("--all").unwrap().count() > 0,
            packages: args
                .get_mut("--package")
                .unwrap()
                .take_multiple()
                .into_iter()
                .map(CargoPackageSpec::new)
                .collect::<Result<_>>()?,
        })
    }
}

/// Configuration information for cargo-component.
///
/// This is used to configure the behavior of cargo-component.
#[derive(Debug)]
pub struct Config {
    /// The package configuration to use
    pub pkg_config: wasm_pkg_client::Config,
    /// The terminal to use.
    terminal: Terminal,
}

impl Config {
    /// Create a new `Config` with the given terminal.
    pub async fn new(terminal: Terminal, config_path: Option<PathBuf>) -> Result<Self> {
        let pkg_config = match config_path {
            Some(path) => wasm_pkg_client::Config::from_file(path).await?,
            None => wasm_pkg_client::Config::global_defaults().await?,
        };
        Ok(Self {
            pkg_config,
            terminal,
        })
    }

    /// Gets the package configuration.
    pub fn pkg_config(&self) -> &wasm_pkg_client::Config {
        &self.pkg_config
    }

    /// Gets a reference to the terminal for writing messages.
    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    /// Creates a [`Client`] from this configuration.
    pub async fn client(
        &self,
        cache: Option<PathBuf>,
        offline: bool,
    ) -> anyhow::Result<Arc<CachingClient<FileCache>>> {
        Ok(Arc::new(CachingClient::new(
            (!offline).then(|| Client::new(self.pkg_config.clone())),
            FileCache::new(cache_dir(cache)?).await?,
        )))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::iter::empty;

    #[test]
    fn it_parses_flags() {
        let mut args = Args::default().flag("--flag", Some('f'));

        // Test not the flag
        args.parse("--not-flag", &mut empty::<String>()).unwrap();
        let arg = args.get("--flag").unwrap();
        assert_eq!(arg.count(), 0);

        // Test the flag
        args.parse("--flag", &mut empty::<String>()).unwrap();
        assert_eq!(
            args.parse("--flag", &mut empty::<String>())
                .unwrap_err()
                .to_string(),
            "the argument '--flag' cannot be used multiple times"
        );
        let arg = args.get_mut("--flag").unwrap();
        assert_eq!(arg.count(), 1);
        arg.reset();

        // Test not the short flag
        args.parse("-rxd", &mut empty::<String>()).unwrap();
        let arg = args.get("--flag").unwrap();
        assert_eq!(arg.count(), 0);

        // Test the short flag
        args.parse("-rfx", &mut empty::<String>()).unwrap();
        assert_eq!(
            args.parse("-fxz", &mut empty::<String>())
                .unwrap_err()
                .to_string(),
            "the argument '--flag' cannot be used multiple times"
        );
        let arg = args.get("--flag").unwrap();
        assert_eq!(arg.count(), 1);

        // Test it prints correctly
        assert_eq!(arg.to_string(), "--flag")
    }

    #[test]
    fn it_parses_single_values() {
        let mut args = Args::default().single("--option", "VALUE", Some('o'));

        // Test not the option
        args.parse("--not-option", &mut empty::<String>()).unwrap();
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(arg.take_single(), None);

        // Test missing value
        assert_eq!(
            args.parse("--option", &mut empty::<String>())
                .unwrap_err()
                .to_string(),
            "a value is required for '--option <VALUE>' but none was supplied"
        );

        // Test the option with equals
        args.parse("--option=value", &mut empty::<String>())
            .unwrap();
        assert_eq!(
            args.parse("--option=value", &mut empty::<String>())
                .unwrap_err()
                .to_string(),
            "the argument '--option <VALUE>' cannot be used multiple times"
        );
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(arg.take_single(), Some("value".to_string()));
        arg.reset();

        // Test the option with space
        let mut iter = ["value".to_string()].into_iter();
        args.parse("--option", &mut iter).unwrap();
        assert!(iter.next().is_none());
        let mut iter = ["value".to_string()].into_iter();
        assert_eq!(
            args.parse("--option", &mut iter).unwrap_err().to_string(),
            "the argument '--option <VALUE>' cannot be used multiple times"
        );
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(arg.take_single(), Some("value".to_string()));
        arg.reset();

        // Test not the short option
        args.parse("-xyz", &mut empty::<String>()).unwrap();
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(arg.take_single(), None);

        assert_eq!(
            args.parse("-fo", &mut empty::<String>())
                .unwrap_err()
                .to_string(),
            "a value is required for '--option <VALUE>' but none was supplied"
        );

        // Test the short option without equals
        args.parse("-xofoo", &mut empty::<String>()).unwrap();
        assert_eq!(
            args.parse("-zyobar", &mut iter).unwrap_err().to_string(),
            "the argument '--option <VALUE>' cannot be used multiple times"
        );
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(arg.take_single(), Some(String::from("foo")));

        // Test the short option with equals
        args.parse("-xo=foo", &mut empty::<String>()).unwrap();
        assert_eq!(
            args.parse("-zyo=bar", &mut iter).unwrap_err().to_string(),
            "the argument '--option <VALUE>' cannot be used multiple times"
        );
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(arg.take_single(), Some(String::from("foo")));

        // Test the short option with space
        let mut iter = ["value".to_string()].into_iter();
        args.parse("-xo", &mut iter).unwrap();
        let mut iter = ["value".to_string()].into_iter();
        assert_eq!(
            args.parse("-zyo", &mut iter).unwrap_err().to_string(),
            "the argument '--option <VALUE>' cannot be used multiple times"
        );
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(arg.take_single(), Some(String::from("value")));

        // Test it prints correctly
        assert_eq!(arg.to_string(), "--option <VALUE>")
    }

    #[test]
    fn it_parses_multiple_values() {
        let mut args = Args::default().multiple("--option", "VALUE", Some('o'));

        // Test not the option
        args.parse("--not-option", &mut empty::<String>()).unwrap();
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(arg.take_multiple(), Vec::<String>::new());

        // Test missing value
        assert_eq!(
            args.parse("--option", &mut empty::<String>())
                .unwrap_err()
                .to_string(),
            "a value is required for '--option <VALUE>' but none was supplied"
        );

        // Test the option with equals
        args.parse("--option=foo", &mut empty::<String>()).unwrap();
        args.parse("--option=bar", &mut empty::<String>()).unwrap();
        args.parse("--option=baz", &mut empty::<String>()).unwrap();
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(
            arg.take_multiple(),
            vec!["foo".to_string(), "bar".to_string(), "baz".to_string(),]
        );
        arg.reset();

        // Test the option with space
        let mut iter = ["foo".to_string()].into_iter();
        args.parse("--option", &mut iter).unwrap();
        assert!(iter.next().is_none());
        let mut iter = ["bar".to_string()].into_iter();
        args.parse("--option", &mut iter).unwrap();
        assert!(iter.next().is_none());
        let mut iter = ["baz".to_string()].into_iter();
        args.parse("--option", &mut iter).unwrap();
        assert!(iter.next().is_none());
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(
            arg.take_multiple(),
            vec!["foo".to_string(), "bar".to_string(), "baz".to_string(),]
        );
        arg.reset();

        // Test not the short option
        args.parse("-xyz", &mut empty::<String>()).unwrap();
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(arg.take_single(), None);

        // Test missing shot option value
        assert_eq!(
            args.parse("-fo", &mut empty::<String>())
                .unwrap_err()
                .to_string(),
            "a value is required for '--option <VALUE>' but none was supplied"
        );

        // Test the short option without equals
        args.parse("-xofoo", &mut empty::<String>()).unwrap();
        args.parse("-yobar", &mut empty::<String>()).unwrap();
        args.parse("-zobaz", &mut empty::<String>()).unwrap();
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(
            arg.take_multiple(),
            vec!["foo".to_string(), "bar".to_string(), "baz".to_string(),]
        );

        // Test the short option with equals
        args.parse("-xo=foo", &mut empty::<String>()).unwrap();
        args.parse("-yo=bar", &mut empty::<String>()).unwrap();
        args.parse("-zo=baz", &mut empty::<String>()).unwrap();
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(
            arg.take_multiple(),
            vec!["foo".to_string(), "bar".to_string(), "baz".to_string(),]
        );

        // Test the short option with space
        let mut iter = ["foo".to_string()].into_iter();
        args.parse("-xo", &mut iter).unwrap();
        let mut iter = ["bar".to_string()].into_iter();
        args.parse("-yo", &mut iter).unwrap();
        let mut iter = ["baz".to_string()].into_iter();
        args.parse("-zo", &mut iter).unwrap();
        let arg = args.get_mut("--option").unwrap();
        assert_eq!(
            arg.take_multiple(),
            vec!["foo".to_string(), "bar".to_string(), "baz".to_string(),]
        );

        // Test it prints correctly
        assert_eq!(arg.to_string(), "--option <VALUE>")
    }

    #[test]
    fn it_parses_counting_flag() {
        let mut args = Args::default().counting("--flag", Some('f'));

        // Test not the the flag
        args.parse("--not-flag", &mut empty::<String>()).unwrap();
        let arg = args.get("--flag").unwrap();
        assert_eq!(arg.count(), 0);

        // Test the flag
        args.parse("--flag", &mut empty::<String>()).unwrap();
        args.parse("--flag", &mut empty::<String>()).unwrap();
        args.parse("--flag", &mut empty::<String>()).unwrap();
        let arg = args.get_mut("--flag").unwrap();
        assert_eq!(arg.count(), 3);
        arg.reset();

        // Test the short flag
        args.parse("-xfzf", &mut empty::<String>()).unwrap();
        args.parse("-pfft", &mut empty::<String>()).unwrap();
        args.parse("-abcd", &mut empty::<String>()).unwrap();
        let arg = args.get_mut("--flag").unwrap();
        assert_eq!(arg.count(), 4);

        // Test it prints correctly
        assert_eq!(arg.to_string(), "--flag")
    }

    #[test]
    fn it_parses_cargo_arguments() {
        let args: CargoArguments =
            CargoArguments::parse_from(["component", "build", "--workspace"].into_iter()).unwrap();
        assert_eq!(
            args,
            CargoArguments {
                color: None,
                verbose: 0,
                help: false,
                quiet: false,
                targets: Vec::new(),
                manifest_path: None,
                message_format: None,
                release: false,
                frozen: false,
                locked: false,
                offline: false,
                workspace: true,
                packages: Vec::new(),
            }
        );

        let args = CargoArguments::parse_from(
            [
                "component",
                "publish",
                "--help",
                "-vvv",
                "--color=auto",
                "--manifest-path",
                "Cargo.toml",
                "--message-format",
                "json-render-diagnostics",
                "--release",
                "--package",
                "package1",
                "-p=package2@1.1.1",
                "--target=foo",
                "--target",
                "bar",
                "--quiet",
                "--frozen",
                "--locked",
                "--offline",
                "--all",
                "--not-an-option",
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(
            args,
            CargoArguments {
                color: Some(Color::Auto),
                verbose: 3,
                help: true,
                quiet: true,
                targets: vec!["foo".to_string(), "bar".to_string()],
                manifest_path: Some("Cargo.toml".into()),
                message_format: Some("json-render-diagnostics".into()),
                release: true,
                frozen: true,
                locked: true,
                offline: true,
                workspace: true,
                packages: vec![
                    CargoPackageSpec {
                        name: "package1".to_string(),
                        version: None
                    },
                    CargoPackageSpec {
                        name: "package2".to_string(),
                        version: Some(Version::parse("1.1.1").unwrap())
                    }
                ],
            }
        );
    }
}
