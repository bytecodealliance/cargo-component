use anyhow::Result;
use cargo_metadata::Package;
use git2::{ErrorClass, ErrorCode, Repository};

#[derive(Debug)]
pub struct GitMetadata {
    commit: String,
}

impl GitMetadata {
    /// Creates a new Git metadata for the given cargo package.
    pub fn from_package(package: &Package) -> Result<Option<Self>> {
        log::debug!(
            "searching for git metadata from manifest `{path}`",
            path = package.manifest_path
        );

        let repository = match Repository::discover(package.manifest_path.clone()) {
            Ok(repository) => Ok(repository),
            Err(ref e)
                if e.class() == ErrorClass::Repository && e.code() == ErrorCode::NotFound =>
            {
                return Ok(None)
            }
            Err(e) => Err(e),
        }?;

        let head = match repository.head() {
            Ok(head) => Ok(head),
            Err(ref e)
                if e.class() == ErrorClass::Reference && e.code() == ErrorCode::UnbornBranch =>
            {
                return Ok(None)
            }
            Err(error) => Err(error),
        }?;

        let commit = head.peel_to_commit()?;
        let commit_id = commit.id();

        let metadata = Self {
            commit: commit_id.to_string(),
        };

        Ok(Some(metadata))
    }

    pub fn commit(&self) -> &str {
        &self.commit
    }
}
