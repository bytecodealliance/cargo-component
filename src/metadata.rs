//! Module for component metadata representation in `Cargo.toml`.

use anyhow::{bail, Context, Result};
use cargo_component_core::registry::{Dependency, RegistryPackage};
use cargo_metadata::Package;
use semver::{Version, VersionReq};
use serde::{
    de::{self, value::MapAccessDeserializer},
    Deserialize,
};
use serde_json::from_value;
use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    str::FromStr,
    time::SystemTime,
};
use url::Url;
use warg_protocol::registry::PackageName;

/// The default directory to look for a target WIT file.
pub const DEFAULT_WIT_DIR: &str = "wit";

/// The supported ownership model for generated types.
#[derive(Default, Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ownership {
    /// Generated types will be composed entirely of owning fields, regardless
    /// of whether they are used as parameters to imports or not.
    #[default]
    Owning,
    /// Generated types used as parameters to imports will be "deeply
    /// borrowing", i.e. contain references rather than owned values when
    /// applicable.
    Borrowing,
    /// Generate "duplicate" type definitions for a single WIT type, if necessary.
    /// For example if it's used as both an import and an export, or if it's used
    /// both as a parameter to an import and a return value from an import.
    BorrowingDuplicateIfNecessary,
}

impl FromStr for Ownership {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "owning" => Ok(Self::Owning),
            "borrowing" => Ok(Self::Borrowing),
            "borrowing-duplicate-if-necessary" => Ok(Self::BorrowingDuplicateIfNecessary),
            _ => Err(format!(
                "unrecognized ownership: `{s}`; \
                 expected `owning`, `borrowing`, or `borrowing-duplicate-if-necessary`"
            )),
        }
    }
}

/// Configuration for bindings generation.
#[derive(Default, Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Bindings {
    /// The path to the type that implements the target world.
    ///
    /// If `None`, a type named `Component` will be used.
    pub implementor: Option<String>,
    /// A map of resource names to implementing types.
    ///
    /// An example would be "foo:bar/baz/res" => "MyResource".
    pub resources: HashMap<String, String>,
    /// The ownership model for generated types.
    pub ownership: Ownership,
    /// Additional derives to apply to generated binding types.
    pub derives: Vec<String>,
    /// If true, code generation should qualify any features that depend on
    /// `std` with `cfg(feature = "std")`.
    pub std_feature: bool,
}

/// The target of a component.
///
/// The target defines the world of the component being developed.
#[derive(Debug, Clone)]
pub enum Target {
    /// The target is a world from a registry package.
    Package {
        /// The name of the target package (e.g. `wasi:http`).
        name: PackageName,
        /// The registry package being targeted.
        package: RegistryPackage,
        /// The name of the world being targeted.
        ///
        /// [Resolve::select_world][select-world] will be used
        /// to select world.
        ///
        /// [select-world]: https://docs.rs/wit-parser/latest/wit_parser/struct.Resolve.html#method.select_world
        world: Option<String>,
    },
    /// The target is a world from a local wit document.
    Local {
        /// The path to the wit document defining the world.
        ///
        /// Defaults to the `wit` directory.
        path: Option<PathBuf>,
        /// The name of the world being targeted.
        ///
        /// [Resolve::select_world][select-world] will be used
        /// to select world.
        ///
        /// [select-world]: https://docs.rs/wit-parser/latest/wit_parser/struct.Resolve.html#method.select_world
        world: Option<String>,
        /// The dependencies of the wit document being targeted.
        dependencies: HashMap<PackageName, Dependency>,
    },
}

impl Target {
    /// Gets the dependencies of the target.
    pub fn dependencies(&self) -> Cow<HashMap<PackageName, Dependency>> {
        match self {
            Self::Package { name, package, .. } => Cow::Owned(HashMap::from_iter([(
                name.clone(),
                Dependency::Package(package.clone()),
            )])),
            Self::Local { dependencies, .. } => Cow::Borrowed(dependencies),
        }
    }

    /// Gets the target world, if any.
    pub fn world(&self) -> Option<&str> {
        match self {
            Self::Package { world, .. } | Self::Local { world, .. } => world.as_deref(),
        }
    }
}

impl Default for Target {
    fn default() -> Self {
        Self::Local {
            path: None,
            world: None,
            dependencies: HashMap::new(),
        }
    }
}

impl FromStr for Target {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (name, version) = match s.split_once('@') {
            Some((name, version)) => (
                name,
                version
                    .parse()
                    .with_context(|| format!("invalid target version `{version}`"))?,
            ),
            None => bail!("expected target format `<package-name>[/<world>]@<version>`"),
        };

        let (name, world) = match name.split_once('/') {
            Some((name, world)) => {
                wit_parser::validate_id(world)
                    .with_context(|| format!("invalid target world name `{world}`"))?;
                (name, Some(world.to_string()))
            }
            None => (name, None),
        };

        Ok(Self::Package {
            name: name.parse()?,
            package: RegistryPackage {
                name: None,
                version,
                registry: None,
            },
            world,
        })
    }
}

impl<'de> Deserialize<'de> for Target {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Target;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(formatter, "a string or a table")
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Target::from_str(s).map_err(de::Error::custom)
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                #[derive(Default, Deserialize)]
                #[serde(default, deny_unknown_fields)]
                struct Entry {
                    package: Option<String>,
                    version: Option<VersionReq>,
                    world: Option<String>,
                    registry: Option<String>,
                    path: Option<PathBuf>,
                    dependencies: HashMap<PackageName, Dependency>,
                }

                let entry = Entry::deserialize(MapAccessDeserializer::new(map))?;

                match (entry.path, entry.package) {
                    (None, Some(package)) => {
                        for (present, name) in [(!entry.dependencies.is_empty(), "dependencies")] {
                            if present {
                                return Err(de::Error::custom(
                                    format!("cannot specify both `{name}` and `package` fields in a target entry"),
                                ));
                            }
                        }

                        Ok(Target::Package {
                            name: package.parse().map_err(de::Error::custom)?,
                            package: RegistryPackage {
                                name: None,
                                version: entry
                                    .version
                                    .ok_or_else(|| de::Error::missing_field("version"))?,
                                registry: entry.registry,
                            },
                            world: entry.world,
                        })
                    }
                    (path, None) => {
                        for (present, name) in [
                            (entry.version.is_some(), "version"),
                            (entry.registry.is_some(), "registry"),
                        ] {
                            if present {
                                return Err(de::Error::custom(
                                    format!("cannot specify both `{name}` and `path` fields in a target entry"),
                                ));
                            }
                        }
                        Ok(Target::Local {
                            path,
                            world: entry.world,
                            dependencies: entry.dependencies,
                        })
                    }
                    (Some(_), Some(_)) => Err(de::Error::custom(
                        "cannot specify both `path` and `package` fields in a target entry",
                    )),
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// Represents the `package.metadata.component` section in `Cargo.toml`.
#[derive(Default, Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComponentSection {
    /// The package name of the component, for publishing.
    pub package: Option<PackageName>,
    /// The world targeted by the component.
    pub target: Target,
    /// The path to the WASI adapter to use.
    pub adapter: Option<PathBuf>,
    /// The dependencies of the component.
    pub dependencies: HashMap<PackageName, Dependency>,
    /// The registries to use for the component.
    pub registries: HashMap<String, Url>,
    /// The configuration for bindings generation.
    pub bindings: Bindings,
    /// Whether to use the built-in `wasi:http/proxy` adapter for the component.
    ///
    /// This should only be `true` when `adapter` is None.
    pub proxy: bool,
}

/// Represents cargo metadata for a WebAssembly component.
#[derive(Debug, Clone)]
pub struct ComponentMetadata {
    /// The crate name.
    pub name: String,
    /// The version of the crate.
    pub version: Version,
    /// The path to the cargo manifest file.
    pub manifest_path: PathBuf,
    /// The last modified time of the manifest file.
    pub modified_at: SystemTime,
    /// The component section in `Cargo.toml`.
    pub section: ComponentSection,
    /// Whether the component section was present in `Cargo.toml`.
    pub section_present: bool,
}

impl ComponentMetadata {
    /// Creates a new component metadata for the given cargo package.
    pub fn from_package(package: &Package) -> Result<Self> {
        log::debug!(
            "searching for component metadata in manifest `{path}`",
            path = package.manifest_path
        );

        let mut section_present = false;
        let mut section: ComponentSection = match package.metadata.get("component").cloned() {
            Some(component) => {
                section_present = true;
                from_value(component).with_context(|| {
                    format!(
                        "failed to deserialize component metadata from `{path}`",
                        path = package.manifest_path
                    )
                })?
            }
            None => {
                log::debug!(
                    "manifest `{path}` has no component metadata",
                    path = package.manifest_path
                );
                Default::default()
            }
        };

        let manifest_dir = package
            .manifest_path
            .parent()
            .map(|p| p.as_std_path())
            .with_context(|| {
                format!(
                    "manifest path `{path}` has no parent directory",
                    path = package.manifest_path
                )
            })?;
        let modified_at = crate::last_modified_time(package.manifest_path.as_std_path())?;

        // Make all paths stored in the metadata relative to the manifest directory.
        if let Target::Local {
            path, dependencies, ..
        } = &mut section.target
        {
            if let Some(path) = path {
                *path = manifest_dir.join(path.as_path());
            }

            for dependency in dependencies.values_mut() {
                if let Dependency::Local(path) = dependency {
                    *path = manifest_dir.join(path.as_path());
                }
            }
        }

        for dependency in section.dependencies.values_mut() {
            if let Dependency::Local(path) = dependency {
                *path = manifest_dir.join(path.as_path());
            }
        }

        if let Some(adapter) = section.adapter.as_mut() {
            *adapter = manifest_dir.join(adapter.as_path());
        }

        Ok(Self {
            name: package.name.clone(),
            version: package.version.clone(),
            manifest_path: package.manifest_path.clone().into(),
            modified_at,
            section,
            section_present,
        })
    }

    /// Gets the target package name.
    ///
    /// Returns `None` if the target is not a registry package.
    pub fn target_package(&self) -> Option<&PackageName> {
        match &self.section.target {
            Target::Package { name, .. } => Some(name),
            _ => None,
        }
    }

    /// Gets the path to a local target.
    ///
    /// Returns `None` if the target is a registry package or
    /// if a path is not specified and the default path does not exist.
    pub fn target_path(&self) -> Option<Cow<Path>> {
        match &self.section.target {
            Target::Local {
                path: Some(path), ..
            } => Some(path.into()),
            Target::Local { path: None, .. } => {
                let path = self.manifest_path.parent().unwrap().join(DEFAULT_WIT_DIR);

                if path.exists() {
                    Some(path.into())
                } else {
                    None
                }
            }
            Target::Package { .. } => None,
        }
    }

    /// Gets the target world.
    ///
    /// Returns `None` if there is no target world.
    pub fn target_world(&self) -> Option<&str> {
        self.section.target.world()
    }
}
