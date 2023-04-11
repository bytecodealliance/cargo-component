use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::*, str::contains};
use std::fs;
use toml_edit::{value, Document};

mod support;

#[test]
fn help() {
    for arg in ["help add", "add -h", "add --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains("Add a dependency for a WebAssembly component"))
            .success();
    }
}

#[test]
fn requires_name_and_package() {
    cargo_component("add")
        .assert()
        .stderr(contains("cargo component add <NAME> <PACKAGE>"))
        .failure();
}

#[test]
fn validate_name_does_not_conflict_with_package() -> Result<()> {
    let project = Project::new("foo")?;
    project
        .cargo_component("add foo bar")
        .assert()
        .stderr(contains(
            "cannot add dependency `foo` as it conflicts with the component's package name",
        ))
        .failure();

    Ok(())
}

#[tokio::test]
async fn validate_the_package_exists() -> Result<()> {
    let (_server, config) = start_warg_server().await?;

    let root = create_root()?;
    config.write_to_file(&root.join("warg-config.json"))?;

    let project = Project::with_root(&root, "foo", "")?;

    project
        .cargo_component("add bar foo/bar")
        .assert()
        .stderr(contains("package `foo/bar` not found"))
        .failure();

    Ok(())
}

#[tokio::test]
async fn validate_the_version_exists() -> Result<()> {
    let (_server, config) = start_warg_server().await?;

    let root = create_root()?;
    config.write_to_file(&root.join("warg-config.json"))?;

    publish_component(&config, "foo/bar", "1.1.0", "(component)", true).await?;

    let project = Project::with_root(&root, "foo", "")?;

    project
        .cargo_component("add bar foo/bar")
        .assert()
        .stderr(contains("Added dependency `bar` with version `1.1.0`"))
        .success();

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;
    assert!(contains(r#"bar = "foo/bar@1.1.0""#).eval(&manifest));

    project
        .cargo_component("add --version 2.0.0 baz foo/bar")
        .assert()
        .stderr(contains(
            "component package `foo/bar` has no release matching version requirement `^2.0.0`",
        ))
        .failure();

    Ok(())
}

#[test]
fn checks_for_duplicate_dependencies() -> Result<()> {
    let project = Project::new("foo")?;
    let manifest_path = project.root().join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)?;
    let mut doc: Document = manifest.parse()?;
    doc["package"]["metadata"]["component"]["dependencies"]["bar"] = value("foo/bar@1.2.3");
    fs::write(manifest_path, doc.to_string())?;

    project
        .cargo_component("add bar foo/bar")
        .assert()
        .stderr(contains(
            "cannot add dependency `bar` as it conflicts with an existing dependency",
        ))
        .failure();

    Ok(())
}

#[tokio::test]
async fn prints_modified_manifest_for_dry_run() -> Result<()> {
    let (_server, config) = start_warg_server().await?;

    let root = create_root()?;
    config.write_to_file(&root.join("warg-config.json"))?;

    publish_component(&config, "foo/bar", "1.2.3", "(component)", true).await?;

    let project = Project::with_root(&root, "foo", "")?;

    project
        .cargo_component("add --dry-run bar foo/bar")
        .assert()
        .stderr(contains(r#"Added dependency `bar` with version `1.2.3`"#))
        .success();

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;

    // Assert the dependency was added to the manifest
    assert!(!contains(r#"bar = "foo/baz@1.2.3""#).eval(&manifest));

    Ok(())
}
