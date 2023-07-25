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
use std::{borrow::Cow, collections::HashMap, path::PathBuf, str::FromStr, time::SystemTime};
use url::Url;
use warg_protocol::registry::PackageId;

/// The target of a component.
///
/// The target defines the world of the component being developed.
#[derive(Debug, Clone)]
pub enum Target {
    /// The target is a world from a registry package.
    Package {
        /// The id of the target package (e.g. `wasi:http`).
        id: PackageId,
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
        path: PathBuf,
        /// The name of the world being targeted.
        ///
        /// [Resolve::select_world][select-world] will be used
        /// to select world.
        ///
        /// [select-world]: https://docs.rs/wit-parser/latest/wit_parser/struct.Resolve.html#method.select_world
        world: Option<String>,
        /// The dependencies of the wit document being targeted.
        dependencies: HashMap<PackageId, Dependency>,
    },
}

impl Target {
    /// Gets the dependencies of the target.
    pub fn dependencies(&self) -> Cow<HashMap<PackageId, Dependency>> {
        match self {
            Self::Package { id, package, .. } => Cow::Owned(HashMap::from_iter([(
                id.clone(),
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

impl FromStr for Target {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (id, version) = match s.split_once('@') {
            Some((id, version)) => (
                id,
                version
                    .parse()
                    .with_context(|| format!("invalid target version `{version}`"))?,
            ),
            None => bail!("expected target format `<package-id>[/<world>]@<version>`"),
        };

        let (id, world) = match id.split_once('/') {
            Some((id, world)) => {
                wit_parser::validate_id(world)
                    .with_context(|| format!("invalid target world name `{world}`"))?;
                (id, Some(world.to_string()))
            }
            None => (id, None),
        };

        Ok(Self::Package {
            id: id.parse()?,
            package: RegistryPackage {
                id: None,
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
                    dependencies: HashMap<PackageId, Dependency>,
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
                            id: package.parse().map_err(de::Error::custom)?,
                            package: RegistryPackage {
                                id: None,
                                version: entry
                                    .version
                                    .ok_or_else(|| de::Error::missing_field("version"))?,
                                registry: entry.registry,
                            },
                            world: entry.world,
                        })
                    }
                    (Some(path), None) => {
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
                    (None, None) => Err(de::Error::missing_field("package")),
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
    /// The package id of the component, for publishing.
    pub package: Option<PackageId>,
    /// The world targeted by the component.
    pub target: Option<Target>,
    /// The dependencies of the component.
    pub dependencies: HashMap<PackageId, Dependency>,
    /// The registries to use for the component.
    pub registries: HashMap<String, Url>,
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
}

impl ComponentMetadata {
    /// Creates a new component metadata for the given cargo package.
    ///
    /// Returns `Ok(None)` if the package does not have a `component` section.
    pub fn from_package(package: &Package) -> Result<Option<Self>> {
        log::debug!(
            "searching for component metadata in manifest `{path}`",
            path = package.manifest_path
        );

        let mut section: ComponentSection = match package.metadata.get("component").cloned() {
            Some(component) => from_value(component)?,
            None => {
                log::debug!(
                    "manifest `{path}` has no component metadata",
                    path = package.manifest_path
                );
                return Ok(None);
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
        let modified_at = crate::last_modified_time(&package.manifest_path)?;

        // Make all paths stored in the metadata relative to the manifest directory.
        if let Some(Target::Local {
            path, dependencies, ..
        }) = &mut section.target
        {
            *path = manifest_dir.join(path.as_path());

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

        Ok(Some(Self {
            name: package.name.clone(),
            version: package.version.clone(),
            manifest_path: package.manifest_path.clone().into(),
            modified_at,
            section,
        }))
    }
}
