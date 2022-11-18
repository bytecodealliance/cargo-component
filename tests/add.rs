use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::*, str::contains};
use std::fs;
use toml_edit::{value, Document, InlineTable, Value};

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

#[test]
fn validate_name_does_not_conflict_with_package() -> Result<()> {
    let project = Project::new("foo")?;
    project
        .cargo_component("add bar/foo")
        .assert()
        .stderr(contains(
            "cannot add dependency `foo` as it conflicts with the component's package name",
        ))
        .failure();

    Ok(())
}

#[test]
fn validate_the_package_exists() -> Result<()> {
    let project = Project::new("foo")?;
    let manifest_path = project.root().join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)?;
    let mut doc: Document = manifest.parse()?;
    doc["package"]["metadata"]["component"]["registries"]["default"] = value(
        InlineTable::from_iter([("path", Value::from("registry"))].into_iter()),
    );
    fs::write(manifest_path, doc.to_string())?;

    project
        .cargo_component("registry new registry")
        .assert()
        .stderr(contains("Creating local component registry"))
        .success();

    project
        .cargo_component("add foo/bar")
        .assert()
        .stderr(contains(
            "package `foo/bar` does not exist in local registry",
        ))
        .failure();

    Ok(())
}

#[test]
fn validate_the_version_exists() -> Result<()> {
    let project = Project::new("foo")?;
    let manifest_path = project.root().join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)?;
    let mut doc: Document = manifest.parse()?;
    doc["package"]["metadata"]["component"]["registries"]["default"] = value(
        InlineTable::from_iter([("path", Value::from("registry"))].into_iter()),
    );
    fs::write(manifest_path, doc.to_string())?;

    project
        .cargo_component("registry publish -r registry --id foo/bar -v 1.0.0 world.wit")
        .assert()
        .stderr(contains("Publishing version 1.0.0 of package `foo/bar`"))
        .success();

    project
        .cargo_component("add --version 2.0.0 foo/bar")
        .assert()
        .stderr(contains(
            "package `foo/bar` has no release that satisfies version requirement `^2.0.0`",
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
    doc["package"]["metadata"]["component"]["dependencies"]["bar"] = value("foo/bar:1.2.3");
    fs::write(manifest_path, doc.to_string())?;

    project
        .cargo_component("add foo/bar")
        .assert()
        .stderr(contains(
            "cannot add dependency `bar` as it conflicts with an existing dependency",
        ))
        .failure();

    Ok(())
}

#[test]
fn prints_modified_manifest_for_dry_run() -> Result<()> {
    let project = Project::new("foo")?;
    let manifest_path = project.root().join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)?;
    let mut doc: Document = manifest.parse()?;
    doc["package"]["metadata"]["component"]["registries"]["default"] = value(
        InlineTable::from_iter([("path", Value::from("registry"))].into_iter()),
    );
    fs::write(manifest_path, doc.to_string())?;

    project
        .cargo_component("registry publish -r registry --id foo/bar -v 1.2.3 world.wit")
        .assert()
        .stderr(contains("Publishing version 1.2.3 of package `foo/bar`"))
        .success();

    project
        .cargo_component("add --dry-run foo/bar")
        .assert()
        .stderr(contains(r#"Added dependency `bar` with version `1.2.3`"#))
        .success();

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;

    // Assert the dependency was added to the manifest
    assert!(!contains(r#"bar = "foo/baz:1.2.3""#).eval(&manifest));

    Ok(())
}
