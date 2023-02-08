//! Module for interacting with package log files.
use crate::metadata::PackageId;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use std::{fmt, fs, path::Path};
use warg_protocol::{
    package::{PackageRecord, Validator},
    ProtoEnvelope, ProtoEnvelopeBody,
};

/// The currently supported package log file version.
const PACKAGE_LOG_VERSION: u32 = 1;

fn deserialize_validator<'de, D>(deserializer: D) -> Result<Validator, D::Error>
where
    D: Deserializer<'de>,
{
    let v: &serde_json::value::RawValue = Deserialize::deserialize(deserializer)?;

    // If the validator fails to deserialize, return a default validator
    Ok(
        Validator::deserialize(&mut serde_json::Deserializer::from_str(v.get()))
            .ok()
            .unwrap_or_else(|| {
                log::debug!(
                    "failed to deserialize validation state; a full validation will be performed"
                );
                Validator::default()
            }),
    )
}

/// Represents the supported package types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageType {
    /// The package is a WebAssembly module.
    Module,
    /// The package is a WIT package.
    WitPackage,
    /// The package is a WebAssembly component.
    Component,
}

impl fmt::Display for PackageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Module => write!(f, "WebAssembly module"),
            Self::WitPackage => write!(f, "WIT package"),
            Self::Component => write!(f, "WebAssembly component"),
        }
    }
}

/// Represents a package log file on disk.
///
/// Package logs are stored in JSON format and contain
/// a list of base64 encoded entries and validation state.
///
/// Each entry is a serialized signed envelope containing
/// a single package log record.
///
/// The validation state is used to avoid re-validating
/// the entire package log on every read.
///
/// If the validation state fails to deserialize, it will be
/// rebuilt from the package log entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageLog {
    version: u32,
    id: PackageId,
    #[serde(rename = "type")]
    ty: PackageType,
    entries: Vec<ProtoEnvelopeBody>,
    #[serde(deserialize_with = "deserialize_validator")]
    validator: Validator,
}

impl PackageLog {
    /// Creates a new package log with the given package type.
    pub fn new(id: PackageId, ty: PackageType) -> Self {
        Self {
            version: PACKAGE_LOG_VERSION,
            id,
            ty,
            entries: Default::default(),
            validator: Default::default(),
        }
    }

    /// Opens an existing package log.
    ///
    /// If `validate` is `true`, the package log entries will be validated.
    pub fn open(path: impl AsRef<Path>, validate: bool) -> Result<Self> {
        let path = path.as_ref();

        log::debug!("opening package log `{path}`", path = path.display());

        let bytes = fs::read(path).with_context(|| {
            format!("failed to read package log `{path}`", path = path.display())
        })?;

        let mut log: Self = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "failed to deserialize package log `{path}`",
                path = path.display()
            )
        })?;

        if log.version != PACKAGE_LOG_VERSION {
            bail!(
                "unsupported version {version} for package log `{path}`",
                version = log.version,
                path = path.display()
            );
        }

        if validate || log.validator.root().is_none() {
            // Perform a full validation of the log
            log.validate().with_context(|| {
                format!(
                    "failed to validate package log file `{path}`",
                    path = path.display()
                )
            })?;
        }

        Ok(log)
    }

    /// Gets the validator of the package log.
    pub fn validator(&self) -> &Validator {
        &self.validator
    }

    /// Gets the package type of the log.
    pub fn package_type(&self) -> PackageType {
        self.ty
    }

    /// Appends a new entry to the package log.
    ///
    /// The given record must validate with the current validation state
    /// of the package log.
    ///
    /// This method consumes the package log and returns a new package log
    /// with the record appended only if the record validates.
    pub fn append(mut self, record: ProtoEnvelope<PackageRecord>) -> Result<Self> {
        self.validator
            .validate(&record)
            .context("failed to validate package log entry being appended")?;

        self.entries.push(record.into());

        Ok(self)
    }

    /// Saves the current package log to disk.
    ///
    /// The package log is updated if it has changed.
    ///
    /// The validation state is always updated.
    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        log::debug!("saving package log `{path}`", path = path.display());

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create directory `{path}`",
                    path = parent.display()
                )
            })?;
        }

        fs::write(path, serde_json::to_string_pretty(self)?).with_context(|| {
            format!(
                "failed to write package log file `{path}`",
                path = path.display()
            )
        })?;

        Ok(())
    }

    fn validate(&mut self) -> Result<()> {
        log::debug!("performing full validation of package log");

        for entry in self.entries.iter().cloned() {
            self.validator.validate(&entry.try_into()?)?;
        }

        Ok(())
    }
}
