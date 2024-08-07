use std::{fs, rc::Rc};

use anyhow::{Context, Result};
use assert_cmd::prelude::*;
use predicates::{prelude::*, str::contains};
use tempfile::TempDir;
use toml_edit::value;
use wasm_pkg_client::warg::WargRegistryConfig;

use crate::support::*;

mod support;

#[test]
fn help() {
    for arg in ["help add", "add -h", "add --help"] {
        cargo_component(arg.split_whitespace())
            .assert()
            .stdout(contains("Add a dependency for a WebAssembly component"))
            .success();
    }
}

#[test]
fn requires_package() {
    cargo_component(["add"])
        .assert()
        .stderr(contains("cargo component add <PACKAGE>"))
        .failure();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn validate_the_package_exists() -> Result<()> {
    let (server, _, _) = spawn_server(["foo"]).await?;

    let project = server.project("foo", true, Vec::<String>::new())?;

    project
        .cargo_component(["add", "foo:bar"])
        .assert()
        .stderr(contains("package `foo:bar` was not found"))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn validate_the_version_exists() -> Result<()> {
    let (server, config, registry) = spawn_server(Vec::<String>::new()).await?;

    // NOTE(thomastaylor312): Once we have publishing in wasm_pkg_client, we won't need to get the config directly like this
    let warg_config =
        WargRegistryConfig::try_from(config.registry_config(&registry).unwrap()).unwrap();

    publish_component(
        &warg_config.client_config,
        "test:bar",
        "1.1.0",
        "(component)",
        true,
    )
    .await?;

    let project = server.project("foo", true, Vec::<String>::new())?;

    project
        .cargo_component(["add", "test:bar"])
        .assert()
        .stderr(contains("Added dependency `test:bar` with version `1.1.0`"))
        .success();

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;
    assert!(contains(r#""test:bar" = "1.1.0""#).eval(&manifest));

    project
        .cargo_component(["add", "--name", "test:bar2", "test:bar@2.0.0"])
        .assert()
        .stderr(contains(
            "component registry package `test:bar` has no release matching version requirement `^2.0.0`",
        ))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn adds_dependencies_to_target_component() -> Result<()> {
    let (server, config, registry) = spawn_server(Vec::<String>::new()).await?;

    let warg_config =
        WargRegistryConfig::try_from(config.registry_config(&registry).unwrap()).unwrap();

    publish_component(
        &warg_config.client_config,
        "test:bar",
        "1.1.0",
        "(component)",
        true,
    )
    .await?;

    let project = server.project("foo_target", true, Vec::<String>::new())?;

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;
    assert!(!contains("package.metadata.component.target.dependencies").eval(&manifest));

    project
        .cargo_component(["add", "test:bar", "--target"])
        .assert()
        .stderr(contains("Added dependency `test:bar` with version `1.1.0`"));

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;
    assert!(contains(r#""test:bar" = "1.1.0""#).eval(&manifest));
    assert!(contains("package.metadata.component.target.dependencies").eval(&manifest));

    project
        .cargo_component(["add", "test:bar", "--target"])
        .assert()
        .stderr(contains(
            "cannot add dependency `test:bar` as it conflicts with an existing dependency",
        ));

    Ok(())
}

#[test]
fn checks_for_duplicate_dependencies() -> Result<()> {
    let project = Project::new("foo", true)?;
    project.update_manifest(|mut doc| {
        doc["package"]["metadata"]["component"]["dependencies"]["foo:bar"] = value("1.2.3");
        Ok(doc)
    })?;

    project
        .cargo_component(["add", "foo:bar"])
        .assert()
        .stderr(contains(
            "cannot add dependency `foo:bar` as it conflicts with an existing dependency",
        ))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn prints_modified_manifest_for_dry_run() -> Result<()> {
    let (server, config, registry) = spawn_server(Vec::<String>::new()).await?;

    let warg_config =
        WargRegistryConfig::try_from(config.registry_config(&registry).unwrap()).unwrap();

    publish_component(
        &warg_config.client_config,
        "test:bar",
        "1.2.3",
        "(component)",
        true,
    )
    .await?;

    let project = server.project("foo", true, Vec::<String>::new())?;

    project
        .cargo_component(["add", "--dry-run", "test:bar"])
        .assert()
        .stderr(contains(
            r#"Added dependency `test:bar` with version `1.2.3`"#,
        ))
        .success();

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;

    // Assert the dependency was added to the manifest
    assert!(!contains(r#"\"test:bar\" = "1.2.3""#).eval(&manifest));

    Ok(())
}

fn validate_add_from_path(project: &Project) -> Result<()> {
    project
        .cargo_component(["add", "--path", "foo/baz", "foo:baz"])
        .assert()
        .stderr(contains("Added dependency `foo:baz` from path `foo/baz`"));

    project
        .cargo_component(["add", "--target", "--path", "foo/qux", "foo:qux"])
        .assert()
        .stderr(contains("Added dependency `foo:qux` from path `foo/qux`"));

    let manifest = fs::read_to_string(project.root().join("Cargo.toml"))?;
    assert!(contains(r#""foo:baz" = { path = "foo/baz" }"#).eval(&manifest));
    assert!(contains(r#""foo:qux" = { path = "foo/qux" }"#).eval(&manifest));
    Ok(())
}

#[test]
fn test_validate_add_from_path() -> Result<()> {
    let project = Project::new("foo", true)?;
    validate_add_from_path(&project)?;
    Ok(())
}

#[test]
fn two_projects_in_one_workspace_validate_add_from_path() -> Result<()> {
    let temp_dir = Rc::new(TempDir::new()?);
    let cargo_workspace = temp_dir.path().join("Cargo.toml");
    fs::write(
        &cargo_workspace,
        r#"
[workspace]
resolver = "2"
"#,
    )
    .with_context(|| {
        format!(
            "failed to write `{cargo_workspace}`",
            cargo_workspace = cargo_workspace.display()
        )
    })?;
    let p1 = Project::with_dir(temp_dir.clone(), "foo", true, Vec::<String>::new())?;
    let p2 = Project::with_dir(temp_dir.clone(), "bar", true, Vec::<String>::new())?;

    validate_add_from_path(&p1)?;
    validate_add_from_path(&p2)?;
    Ok(())
}
