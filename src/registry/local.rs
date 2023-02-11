//! Module for interacting with local file system component registries.
use super::{ContentLocation, Registry, RegistryPackageResolution};
use crate::{
    config::Config,
    log::{PackageLog, PackageType},
    metadata::PackageId,
};
use anyhow::{anyhow, bail, Context, Result};
use cargo::util::{FileLock, Filesystem};
use p256::ecdsa::SigningKey;
use semver::{Version, VersionReq};
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};
use url::Url;
use warg_crypto::{
    hash::{DynHash, HashAlgorithm, Sha256},
    signing::PrivateKey,
};
use warg_protocol::{
    package::{PackageEntry, PackageRecord, ReleaseState, PACKAGE_RECORD_VERSION},
    registry::LogId,
    ProtoEnvelope,
};
use wit_parser::{Resolve, UnresolvedPackage};

const REGISTRY_KEY_FILE_NAME: &str = "local-signing.key";
const PACKAGES_DIRECTORY_NAME: &str = "packages";
const CONTENTS_DIRECTORY_NAME: &str = "contents";

fn generate_signing_key() -> SigningKey {
    SigningKey::random(&mut rand_core::OsRng)
}

/// Simple function to trim leading ASCII whitespace from a byte slice.
/// Copied from `[u8]::trim_ascii_start` as it's not stable yet.
fn trim_ascii_start(mut bytes: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = bytes {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

/// Represents a local component registry.
///
/// Local component registries store locally-published packages and vendored packages
/// from remote registries.
///
/// Local component registries have the following entries in the root directory:
///
/// * `local-signing.key` - The signing key for the local registry; also serves as a lock file.
/// * `packages` - The directory containing the packages.
/// * `contents` - The directory containing package contents.
///
/// Each package log is stored at `./logs/<id>` where `<id>` is the warg protocol log ID.
///
/// Package contents are stored in a `./contents/<algo>/<digest>` file.
///
/// Remote package logs that are vendored into the local registry will be signed
/// by the original private keys; vendored packages do not support appending new
/// records to the log, only appending the entries directly from the remote log.
///
/// Note that remote packages may have missing content for releases to allow
/// vendoring only specific versions of a package.
pub struct LocalRegistry {
    root: Filesystem,
    _signing_key_file: FileLock,
    signing_key: PrivateKey,
}

impl LocalRegistry {
    /// Opens a local component registry at the given root directory.
    ///
    /// The root directory will be created if it does not exist.
    pub fn open(config: &Config, root: impl Into<PathBuf>, must_exist: bool) -> Result<Self> {
        let root = root.into();

        if must_exist && !root.join(REGISTRY_KEY_FILE_NAME).is_file() {
            bail!(
                "local registry `{root}` does not exist",
                root = root.display()
            );
        }

        log::debug!("opening local registry at `{path}`", path = root.display());
        let root = Filesystem::new(root);

        // Attempt a lock on the signing key file (will create an empty one if it does not exist).
        let mut signing_key_file =
            root.open_rw(REGISTRY_KEY_FILE_NAME, config.cargo(), "signing key")?;

        let mut key = Vec::default();
        let signing_key = if signing_key_file.read_to_end(&mut key).with_context(|| {
            format!(
                "failed to read signing key `{path}`",
                path = signing_key_file.path().display()
            )
        })? == 0
        {
            log::debug!(
                "creating new signing key for local registry at `{path}`",
                path = signing_key_file.path().display()
            );
            let signing_key = generate_signing_key();
            signing_key_file
                .write_all(&signing_key.to_bytes())
                .with_context(|| {
                    format!(
                        "failed to write signing key `{path}`",
                        path = signing_key_file.path().display()
                    )
                })?;
            signing_key.into()
        } else {
            SigningKey::from_bytes(&key)
                .with_context(|| {
                    format!(
                        "failed to parse signing key from `{path}`",
                        path = signing_key_file.path().display()
                    )
                })?
                .into()
        };

        Ok(Self {
            root,
            _signing_key_file: signing_key_file,
            signing_key,
        })
    }

    /// Gets the root of the local registry.
    pub fn root(&self) -> &Filesystem {
        &self.root
    }

    /// Validates a package from the local registry.
    ///
    /// This will validate the package's log and the contents from
    /// every released version of the package.
    ///
    /// If the package log validates, the validation state in the
    /// package log will be updated.
    pub fn validate(&self, id: &PackageId) -> Result<()> {
        let path = self.package_log_path(id);
        if !path.exists() {
            bail!(
                "package `{id}` does not exist in local registry `{root}`",
                root = self.root.as_path_unlocked().display()
            );
        }

        log::debug!("validating package `{id}`");

        let mut log = PackageLog::open(&path, true)?;
        let is_remote_log = log
            .validator()
            .public_key(&self.signing_key.public_key().fingerprint())
            .is_none();

        let mut validated = HashSet::new();
        for release in log.validator().releases() {
            if let ReleaseState::Released { content } = &release.state {
                let path = self.contents_path(content);
                if !path.is_file() {
                    if is_remote_log {
                        // It is okay for remote content to not be present
                        // It just means the release hasn't been vendored locally
                        continue;
                    }

                    bail!(
                        "release {version} is missing content with digest `{content}`",
                        version = release.version
                    );
                }

                if !validated.insert(content.to_string()) {
                    continue;
                }

                let bytes = fs::read(&path).with_context(|| {
                    anyhow!(
                        "failed to read package contents `{path}`",
                        path = path.display()
                    )
                })?;

                let found = content.algorithm().digest(&bytes);
                if content != &found {
                    bail!(
                        "content digest mismatch for release {version}: expected `{content}` but found `{found}`",
                        version = release.version
                    );
                }
            }
        }

        // Save the package log (this will update the validation state)
        log.save(&path)?;

        Ok(())
    }

    /// Publish a package into the local registry.
    pub fn publish(&self, id: &PackageId, version: &Version, path: impl AsRef<Path>) -> Result<()> {
        let orig_contents_path = path.as_ref();

        log::debug!(
            "publishing version {version} of package `{id}` from content file `{contents}`",
            contents = orig_contents_path.display()
        );

        // Digest the contents of the package
        let (contents, package_type) = Self::content_bytes(orig_contents_path)?;
        let digest = HashAlgorithm::Sha256.digest(&contents);
        let log_path = self.package_log_path(id);
        let log_exists = log_path.is_file();
        let public_key = self.signing_key.public_key();

        let log = if log_exists {
            let log = PackageLog::open(&log_path, false)?;
            if log
                .validator()
                .public_key(&public_key.fingerprint())
                .is_none()
            {
                bail!("cannot release package `{id}` as it is not signed by this registry");
            }

            let expected_type = log.package_type();
            if package_type != expected_type {
                bail!(
                    "package contents file `{path}` is a {package_type} but package `{id}` was previous published as a {expected_type}",
                    path = orig_contents_path.display(),
                );
            }

            if log.validator().release(version).is_some() {
                bail!("release {version} for package `{id}` already exists");
            }

            log
        } else {
            PackageLog::new(id.clone(), package_type)
        };

        let mut entries = Vec::new();
        if !log_exists {
            entries.push(PackageEntry::Init {
                hash_algorithm: HashAlgorithm::Sha256,
                key: public_key,
            });
        }

        entries.push(PackageEntry::Release {
            version: version.clone(),
            content: digest.clone(),
        });

        let record = PackageRecord {
            prev: log.validator().root().as_ref().map(|r| r.digest.clone()),
            version: PACKAGE_RECORD_VERSION,
            timestamp: SystemTime::now(),
            entries,
        };

        // Append the record to the log
        log.append(ProtoEnvelope::signed_contents(&self.signing_key, record)?)?
            .save(&log_path)?;

        // Store the package contents
        let contents_path = self.contents_path(&digest);

        if let Some(parent) = contents_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create directory `{path}`",
                    path = parent.display()
                )
            })?;
        }

        fs::write(&contents_path, contents).with_context(|| {
            anyhow!(
                "failed to write package contents `{path}`",
                path = contents_path.display()
            )
        })?;

        Ok(())
    }

    /// Yanks a released package from the local registry.
    pub fn yank(&self, id: &PackageId, version: &Version) -> Result<()> {
        let path = self.package_log_path(id);

        log::debug!("yanking version {version} of package `{id}`");

        if !path.exists() {
            bail!("package `{id}` does not exist");
        }

        let log = PackageLog::open(&path, false)?;
        if log
            .validator()
            .public_key(&self.signing_key.public_key().fingerprint())
            .is_none()
        {
            bail!("cannot yank a release for package `{id}` as it is not signed by this registry");
        }

        match log.validator().release(version) {
            Some(r) => {
                if r.yanked() {
                    bail!("release {version} for package `{id}` is already yanked");
                }
            }
            None => bail!("release {version} for package `{id}` does not exist"),
        }

        let record = PackageRecord {
            prev: log.validator().root().as_ref().map(|r| r.digest.clone()),
            version: PACKAGE_RECORD_VERSION,
            timestamp: SystemTime::now(),
            entries: vec![PackageEntry::Yank {
                version: version.clone(),
            }],
        };

        // Append the record to the log
        log.append(ProtoEnvelope::signed_contents(&self.signing_key, record)?)?
            .save(&path)?;

        Ok(())
    }

    fn package_log_path(&self, id: &PackageId) -> PathBuf {
        let id = LogId::package_log::<Sha256>(id.as_ref());

        self.root
            .as_path_unlocked()
            .join(PACKAGES_DIRECTORY_NAME)
            .join(hex::encode(id.as_ref()))
    }

    fn contents_path(&self, content: &DynHash) -> PathBuf {
        let content = content.to_string();
        let (algo, digest) = content.split_once(':').expect("invalid digest format");

        self.root
            .as_path_unlocked()
            .join(CONTENTS_DIRECTORY_NAME)
            .join(algo)
            .join(digest)
    }

    fn content_bytes(path: &Path) -> Result<(Vec<u8>, PackageType)> {
        if path.is_file() {
            let bytes = fs::read(path).with_context(|| {
                anyhow!(
                    "failed to read package contents `{path}`",
                    path = path.display()
                )
            })?;

            // Simple heuristic to detect if the file is in WebAssembly text format
            let bytes = if trim_ascii_start(&bytes).starts_with(&[b'(']) {
                match wat::parse_bytes(&bytes).with_context(|| {
                    anyhow!(
                        "failed to parse package contents `{path}` as WebAssembly text",
                        path = path.display()
                    )
                })? {
                    Cow::Borrowed(_) => bytes,
                    Cow::Owned(bytes) => bytes,
                }
            } else {
                bytes
            };

            // If it's a WebAssembly module or component, we're done
            if bytes.starts_with(&[0x0, b'a', b's', b'm']) {
                let ty = if bytes[4..] == [0x01, 0x00, 0x00, 0x00] {
                    PackageType::Module
                } else {
                    PackageType::Component
                };

                return Ok((bytes, ty));
            }
        }

        // Lastly, try to parse it as a wit document
        let mut resolve = Resolve::new();
        let pkg = UnresolvedPackage::parse_path(path).with_context(|| {
            anyhow!(
                "failed to parse package contents `{path}` as a WIT document",
                path = path.display()
            )
        })?;

        // TODO: support external dependencies
        let id = resolve.push(pkg, &HashMap::new())?;
        Ok((
            wit_component::encode(&resolve, id).with_context(|| {
                anyhow!(
                    "failed to encode package contents `{path}` as a WIT package",
                    path = path.display()
                )
            })?,
            PackageType::WitPackage,
        ))
    }
}

#[async_trait::async_trait]
impl Registry for LocalRegistry {
    async fn synchronize(&self, _packages: &[&PackageId]) -> Result<()> {
        // Local registries require no synchronization
        // Any vendored package in the registry must be manually updated via the CLI
        Ok(())
    }

    fn resolve(
        &self,
        id: &PackageId,
        requirement: &VersionReq,
    ) -> Result<Option<RegistryPackageResolution>> {
        let path = self.package_log_path(id);
        if !path.exists() {
            bail!(
                "package `{id}` does not exist in local registry `{root}`",
                root = self.root.as_path_unlocked().display()
            );
        }

        let log = PackageLog::open(path, false)?;
        let validator = log.validator();
        let is_remote_log = validator
            .public_key(&self.signing_key.public_key().fingerprint())
            .is_none();

        match validator
            .releases()
            .filter_map(|release| match &release.state {
                ReleaseState::Released { content } => {
                    let path = self.contents_path(content);

                    // Ignore remote packages that don't have content files
                    if requirement.matches(&release.version) && (!is_remote_log || path.is_file()) {
                        Some((&release.version, content, path))
                    } else {
                        None
                    }
                }
                ReleaseState::Yanked { .. } => None,
            })
            .max_by(|(a, _, _), (b, _, _)| a.cmp(b))
        {
            Some((version, digest, path)) => Ok(Some(RegistryPackageResolution {
                id: id.clone(),
                requirement: requirement.clone(),
                url: Url::from_file_path(fs::canonicalize(&path).with_context(|| {
                    format!(
                        "failed to canonicalize local registry content path `{path}`",
                        path = path.display()
                    )
                })?)
                .unwrap(),
                version: version.clone(),
                digest: digest.clone(),
                location: ContentLocation::Local(path),
            })),
            None => Ok(None),
        }
    }
}
