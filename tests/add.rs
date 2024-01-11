use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::*, str::contains};
use std::fs;
use toml_edit::value;

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
        .cargo_component("add --id foo:bar2 foo:bar@2.0.0")
        .assert()
        .stderr(contains(
            "component registry package `foo:bar` has no release matching version requirement `^2.0.0`",
        ))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn adds_dependencies_to_target_component() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    publish_component(&config, "foo:bar", "1.1.0", "(component)", true).await?;

    let project = Project::with_root(&root, "foo_target", "")?;

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;
    assert!(!contains("package.metadata.component.target.dependencies").eval(&manifest));

    project
        .cargo_component("add foo:bar --target")
        .assert()
        .stderr(contains("Added dependency `foo:bar` with version `1.1.0`"));

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;
    assert!(contains(r#""foo:bar" = "1.1.0""#).eval(&manifest));
    assert!(contains("package.metadata.component.target.dependencies").eval(&manifest));

    project
        .cargo_component("add foo:bar --target")
        .assert()
        .stderr(contains(
            "cannot add dependency `foo:bar` as it conflicts with an existing dependency",
        ));

    Ok(())
}

#[test]
fn checks_for_duplicate_dependencies() -> Result<()> {
    let project = Project::new("foo")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        doc["package"]["metadata"]["component"]["dependencies"]["foo:bar"] = value("1.2.3");
        Ok(doc)
    })?;

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

fn validate_add_from_path(project: &Project) -> Result<()> {
    project
        .cargo_component("add --path foo/baz foo:baz")
        .assert()
        .stderr(contains("Added dependency `foo:baz` from path `foo/baz`"));

    project
        .cargo_component("add --target --path foo/qux foo:qux")
        .assert()
        .stderr(contains("Added dependency `foo:qux` from path `foo/qux`"));

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;
    assert!(contains(r#""foo:baz" = { path = "foo/baz" }"#).eval(&manifest));
    assert!(contains(r#""foo:qux" = { path = "foo/qux" }"#).eval(&manifest));
    Ok(())
}

#[test]
fn test_validate_add_from_path() -> Result<()> {
    let project = Project::new("foo")?;
    validate_add_from_path(&project)?;
    Ok(())
}

#[test]
fn two_projects_in_one_workspace_validate_add_from_path() -> Result<()> {
    let root = create_root()?;
    let foo = Project::with_root(&root, "foo", "")?;
    let bar = Project::with_root(&root, "bar", "")?;
    validate_add_from_path(&foo)?;
    validate_add_from_path(&bar)?;
    Ok(())
}
