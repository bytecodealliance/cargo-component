use std::fs;

use anyhow::{Context, Result};
use assert_cmd::prelude::*;
use predicates::str::contains;
use semver::Version;
use toml_edit::{value, Array};
use warg_client::{Client, FileSystemClient};
use warg_protocol::registry::PackageName;
use wasm_metadata::LinkType;
use wasm_pkg_client::warg::WargRegistryConfig;

use crate::support::*;

mod support;

#[test]
fn help() {
    for arg in ["help publish", "publish -h", "publish --help"] {
        wit(arg.split_whitespace())
            .assert()
            .stdout(contains("Publish a WIT package to a registry"))
            .success();
    }
}

#[test]
fn it_fails_with_missing_toml_file() -> Result<()> {
    wit(["publish"])
        .assert()
        .stderr(contains(
            "error: failed to find configuration file `wit.toml`",
        ))
        .failure();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_wit_package() -> Result<()> {
    let (server, _, _) = spawn_server(Vec::<String>::new()).await?;

    let project = server.project("foo", Vec::<String>::new())?;
    project.file("baz.wit", "package test:qux;\n")?;
    project
        .wit(["publish", "--init"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:qux` v0.1.0"))
        .success();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_does_a_dry_run_publish() -> Result<()> {
    let (server, config, registry) = spawn_server(Vec::<String>::new()).await?;
    let warg_config =
        WargRegistryConfig::try_from(config.registry_config(&registry).unwrap()).unwrap();

    let project = server.project("foo", Vec::<String>::new())?;
    project.file("baz.wit", "package test:qux;\n")?;
    project
        .wit(["publish", "--init", "--dry-run"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains(
            "warning: not publishing package to the registry due to the --dry-run option",
        ))
        .success();

    let client = FileSystemClient::new_with_config(None, &warg_config.client_config, None).await?;

    assert!(client
        .download(&"test:qux".parse().unwrap(), &"0.1.0".parse().unwrap())
        .await
        .unwrap_err()
        .to_string()
        .contains("package `test:qux` does not exist"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_with_registry_metadata() -> Result<()> {
    let (server, config, registry) = spawn_server(Vec::<String>::new()).await?;
    let warg_config =
        WargRegistryConfig::try_from(config.registry_config(&registry).unwrap()).unwrap();

    let project = server.project("foo", Vec::<String>::new())?;

    let authors = ["Jane Doe <jane@example.com>"];
    let categories = ["wasm"];
    let description = "A test package";
    let license = "Apache-2.0";
    let documentation = "https://example.com/docs";
    let homepage = "https://example.com/home";
    let repository = "https://example.com/repo";

    project.file("baz.wit", "package test:qux;\n")?;

    project.update_manifest(|mut doc| {
        doc["authors"] = value(Array::from_iter(authors));
        doc["categories"] = value(Array::from_iter(categories));
        doc["description"] = value(description);
        doc["license"] = value(license);
        doc["documentation"] = value(documentation);
        doc["homepage"] = value(homepage);
        doc["repository"] = value(repository);
        Ok(doc)
    })?;

    project
        .wit(["publish", "--init"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:qux` v0.1.0"))
        .success();

    let client = Client::new_with_config(None, &warg_config.client_config, None).await?;
    let download = client
        .download_exact(&PackageName::new("test:qux")?, &Version::parse("0.1.0")?)
        .await?;

    let bytes = fs::read(&download.path).with_context(|| {
        format!(
            "failed to read downloaded package `{path}`",
            path = download.path.display()
        )
    })?;

    let metadata = wasm_metadata::RegistryMetadata::from_wasm(&bytes)
        .with_context(|| {
            format!(
                "failed to parse registry metadata from `{path}`",
                path = download.path.display()
            )
        })?
        .expect("missing registry metadata");

    assert_eq!(
        metadata.get_authors().expect("missing authors").as_slice(),
        authors
    );
    assert_eq!(
        metadata
            .get_categories()
            .expect("missing categories")
            .as_slice(),
        categories
    );
    assert_eq!(
        metadata.get_description().expect("missing description"),
        description
    );
    assert_eq!(metadata.get_license().expect("missing license"), license);

    let links = metadata.get_links().expect("missing links");
    assert_eq!(links.len(), 3);

    assert_eq!(
        links
            .iter()
            .find(|link| link.ty == LinkType::Documentation)
            .expect("missing documentation")
            .value,
        documentation
    );
    assert_eq!(
        links
            .iter()
            .find(|link| link.ty == LinkType::Homepage)
            .expect("missing homepage")
            .value,
        homepage
    );
    assert_eq!(
        links
            .iter()
            .find(|link| link.ty == LinkType::Repository)
            .expect("missing repository")
            .value,
        repository
    );

    Ok(())
}
