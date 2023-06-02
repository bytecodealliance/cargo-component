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
fn requires_package() {
    cargo_component("add")
        .assert()
        .stderr(contains("cargo component add <PACKAGE>"))
        .failure();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn validate_the_package_exists() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    let project = Project::with_root(&root, "foo", "")?;

    project
        .cargo_component("add foo:bar")
        .assert()
        .stderr(contains("package `foo:bar` does not exist"))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn validate_the_version_exists() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    publish_component(&config, "foo:bar", "1.1.0", "(component)", true).await?;

    let project = Project::with_root(&root, "foo", "")?;

    project
        .cargo_component("add foo:bar")
        .assert()
        .stderr(contains("Added dependency `foo:bar` with version `1.1.0`"))
        .success();

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;
    assert!(contains(r#""foo:bar" = "1.1.0""#).eval(&manifest));

    project
        .cargo_component("add --version 2.0.0 --id foo:bar2 foo:bar")
        .assert()
        .stderr(contains(
            "component registry package `foo:bar` has no release matching version requirement `^2.0.0`",
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
    doc["package"]["metadata"]["component"]["dependencies"]["foo:bar"] = value("1.2.3");
    fs::write(manifest_path, doc.to_string())?;

    project
        .cargo_component("add foo:bar")
        .assert()
        .stderr(contains(
            "cannot add dependency `foo:bar` as it conflicts with an existing dependency",
        ))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prints_modified_manifest_for_dry_run() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    publish_component(&config, "foo:bar", "1.2.3", "(component)", true).await?;

    let project = Project::with_root(&root, "foo", "")?;

    project
        .cargo_component("add --dry-run foo:bar")
        .assert()
        .stderr(contains(
            r#"Added dependency `foo:bar` with version `1.2.3`"#,
        ))
        .success();

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;

    // Assert the dependency was added to the manifest
    assert!(!contains(r#"\"foo:bar\" = "1.2.3""#).eval(&manifest));

    Ok(())
}
