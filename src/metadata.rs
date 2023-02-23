//! Module for component metadata representation in `Cargo.toml`.

use anyhow::{anyhow, bail, Result};
use semver::{Version, VersionReq};
use serde::{
    de::{self, value::MapAccessDeserializer, IntoDeserializer},
    Deserialize, Serialize,
};
use std::{borrow::Cow, collections::HashMap, fmt, path::PathBuf, str::FromStr, time::SystemTime};
use url::Url;

/// Represents a unique package identifier in a registry.
///
/// This identifier is unique within a specific registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Ord, PartialOrd)]
#[serde(transparent)]
pub struct PackageId(String);

impl PackageId {
    /// Creates a new package id.
    pub fn new(s: impl Into<String>) -> Self {
        s.into().into()
    }
}

impl From<String> for PackageId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for PackageId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for PackageId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{s}", s = self.0)
    }
}

/// Represents a component registry package.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryPackage {
    /// The id of package.
    pub id: PackageId,

    /// The version requirement of the package.
    pub version: VersionReq,

    /// The name of the component registry containing the package.
    ///
    /// If not specified, the default registry is used.
    pub registry: Option<String>,
}

impl FromStr for RegistryPackage {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.rsplit_once('@') {
            Some((id, version)) => Ok(Self {
                id: id.into(),
                version: version.parse()?,
                registry: None,
            }),
            None => bail!("expected package with format `<package-id>@<version>`"),
        }
    }
}

/// Represents a component dependency.
#[derive(Debug, Clone)]
pub enum Dependency {
    /// The dependency is a registry package.
    Package(RegistryPackage),

    /// The dependency is a path to a local file.
    Local(PathBuf),
}

impl<'de> Deserialize<'de> for Dependency {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Dependency;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(formatter, "a string or a table")
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Self::Value::Package(s.parse().map_err(de::Error::custom)?))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                #[derive(Default, Deserialize)]
                #[serde(default, deny_unknown_fields)]
                struct Entry {
                    path: Option<PathBuf>,
                    package: Option<String>,
                    version: Option<VersionReq>,
                    registry: Option<String>,
                }

                let entry = Entry::deserialize(MapAccessDeserializer::new(map))?;

                match (entry.path, entry.package, entry.version, entry.registry) {
                    (Some(path), None, None, None) => Ok(Self::Value::Local(path)),
                    (None, Some(package), Some(version), registry) => {
                        Ok(Self::Value::Package(RegistryPackage {
                            id: package.into(),
                            version,
                            registry,
                        }))
                    }
                    (Some(_), None, Some(_), _) => Err(de::Error::custom(
                        "cannot specify both `path` and `version` fields in a dependency entry",
                    )),
                    (Some(_), None, None, Some(_)) => Err(de::Error::custom(
                        "cannot specify both `path` and `registry` fields in a dependency entry",
                    )),
                    (Some(_), Some(_), _, _) => Err(de::Error::custom(
                        "cannot specify both `path` and `package` fields in a dependency entry",
                    )),
                    (None, None, _, _) => Err(de::Error::missing_field("package")),
                    (None, Some(_), None, _) => Err(de::Error::missing_field("version")),
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// Represents a component registry in the manifest.
#[derive(Debug, Clone)]
pub enum Registry {
    /// The source is a remote registry.
    Remote(Url),
    /// The source is a local file system registry.
    Local(PathBuf),
}

impl<'de> Deserialize<'de> for Registry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Registry;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(formatter, "a string or a table")
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Self::Value::Remote(s.parse().map_err(de::Error::custom)?))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                #[derive(Default, Deserialize)]
                #[serde(default, deny_unknown_fields)]
                struct Entry {
                    path: Option<PathBuf>,
                    url: Option<Url>,
                }

                let entry = Entry::deserialize(MapAccessDeserializer::new(map))?;

                match (entry.path, entry.url) {
                    (Some(path), None) => Ok(Self::Value::Local(path)),
                    (None, Some(url)) => Ok(Self::Value::Remote(url)),
                    (None, None) => Err(de::Error::missing_field("url")),
                    (Some(_), Some(_)) => Err(de::Error::custom(
                        "cannot specify both `path` and `url` fields in a component registry entry",
                    )),
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// The target of a component.
///
/// The target defines the world of the component being developed.
#[derive(Debug, Clone)]
pub enum Target {
    /// The target is a world from a registry package.
    Package {
        /// The package being targeted.
        package: RegistryPackage,
        /// The name of the world being targeted.
        ///
        /// [Resolve::select_world][select-world] will be used
        /// to select world.
        ///
        /// [select-world]: https://docs.rs/wit-parser/0.6.1/wit_parser/struct.Resolve.html#method.select_world
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
        /// [select-world]: https://docs.rs/wit-parser/0.6.1/wit_parser/struct.Resolve.html#method.select_world
        world: Option<String>,
        /// The dependencies of the wit document being targeted.
        dependencies: HashMap<String, Dependency>,
    },
}

impl Target {
    /// Gets the dependencies of the target.
    pub fn dependencies(&self) -> Cow<HashMap<String, Dependency>> {
        match self {
            Self::Package { package, .. } => Cow::Owned(HashMap::from_iter([(
                String::new(),
                Dependency::Package(package.clone()),
            )])),
            Self::Local { dependencies, .. } => Cow::Borrowed(dependencies),
        }
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
                Ok(Self::Value::Package {
                    package: s.parse().map_err(de::Error::custom)?,
                    world: None,
                })
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
                    dependencies: HashMap<String, Dependency>,
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
                            package: RegistryPackage {
                                id: package.into(),
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
    /// The package name of the component, for publishing.
    pub package: Option<String>,
    /// The world targeted by the component.
    pub target: Option<Target>,
    /// The dependencies of the component.
    pub dependencies: HashMap<String, Dependency>,
    /// The registries to use for the component.
    pub registries: HashMap<String, Registry>,
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
    pub fn from_package(package: &cargo::core::Package) -> Result<Option<Self>> {
        let manifest_path = package.manifest_path();
        let manifest_dir = manifest_path.parent().ok_or_else(|| {
            anyhow!(
                "manifest path `{path}` has no parent directory",
                path = manifest_path.display()
            )
        })?;
        let modified_at = crate::last_modified_time(manifest_path)?;

        log::debug!(
            "searching for component metadata in manifest `{path}`",
            path = manifest_path.display()
        );

        let mut section = match package.manifest().custom_metadata() {
            Some(metadata) => match metadata.get("component") {
                Some(component) => {
                    let document = toml_edit::ser::to_document(&component)?;
                    ComponentSection::deserialize(document.into_deserializer())?
                }
                None => return Ok(None),
            },
            None => return Ok(None),
        };

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

        for registry in section.registries.values_mut() {
            if let Registry::Local(path) = registry {
                *path = manifest_dir.join(path.as_path());
            }
        }

        Ok(Some(Self {
            name: package.name().to_string(),
            version: package.version().clone(),
            manifest_path: manifest_path.into(),
            modified_at,
            section,
        }))
    }
}
