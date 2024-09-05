use std::fs;

use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::*, str::contains};

use crate::support::*;

mod support;

#[test]
fn help() {
    for arg in ["help add", "add -h", "add --help"] {
        wit(arg.split_whitespace())
            .assert()
            .stdout(contains(
                "Adds a reference to a WIT package from a registry",
            ))
            .success();
    }
}

#[test]
fn it_fails_with_missing_toml_file() -> Result<()> {
    wit(["add", "foo:bar"])
        .assert()
        .stderr(contains(
            "error: failed to find configuration file `wit.toml`",
        ))
        .failure();
    Ok(())
}

#[test]
fn requires_package() {
    wit(["add"])
        .assert()
        .stderr(contains("wit add <PACKAGE>"))
        .failure();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn validate_the_package_exists() -> Result<()> {
    let (server, _, _) = spawn_server(["foo"]).await?;

    let project = server.project("foo", Vec::<String>::new())?;

    project
        .wit(["add", "foo:bar"])
        .assert()
        .stderr(contains("package `foo:bar` was not found"))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn validate_the_version_exists() -> Result<()> {
    let (server, _, _) = spawn_server(Vec::<String>::new()).await?;

    let project = server.project("foo", Vec::<String>::new())?;
    project.file("foo.wit", "package test:bar;\n")?;
    project
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v0.1.0"))
        .success();

    let project = server.project("bar", Vec::<String>::new())?;
    project
        .wit(["add", "test:bar"])
        .assert()
        .stderr(contains("Added dependency `test:bar` with version `0.1.0`"))
        .success();

    let manifest = fs::read_to_string(project.root().join("wit.toml"))?;
    assert!(contains(r#""test:bar" = "0.1.0""#).eval(&manifest));

    project
        .wit(["add", "--name", "test:bar2", "test:bar@2.0.0"])
        .assert()
        .stderr(contains(
            "component registry package `test:bar` has no release matching version requirement `^2.0.0`",
        ))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn checks_for_duplicate_dependencies() -> Result<()> {
    let (server, _, _) = spawn_server(Vec::<String>::new()).await?;

    let project = server.project("foo", Vec::<String>::new())?;
    project.file("foo.wit", "package test:bar;\n")?;
    project
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v0.1.0"))
        .success();

    let project = server.project("bar", Vec::<String>::new())?;
    project
        .wit(["add", "test:bar"])
        .assert()
        .stderr(contains("Added dependency `test:bar` with version `0.1.0`"))
        .success();

    let manifest = fs::read_to_string(project.root().join("wit.toml"))?;
    assert!(contains(r#""test:bar" = "0.1.0""#).eval(&manifest));

    project
        .wit(["add", "test:bar"])
        .assert()
        .stderr(contains(
            "cannot add dependency `test:bar` as it conflicts with an existing dependency",
        ))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn does_not_modify_manifest_for_dry_run() -> Result<()> {
    let (server, _, _) = spawn_server(Vec::<String>::new()).await?;

    let project = server.project("foo", Vec::<String>::new())?;
    project.file("foo.wit", "package test:bar;\n")?;
    project
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v0.1.0"))
        .success();

    let project = server.project("bar", Vec::<String>::new())?;
    project
        .wit(["add", "test:bar", "--dry-run"])
        .assert()
        .stderr(contains(
            "Would add dependency `test:bar` with version `0.1.0` (dry run)",
        ))
        .success();

    let manifest = fs::read_to_string(project.root().join("wit.toml"))?;
    assert!(!contains("test:bar").eval(&manifest));

    Ok(())
}

#[test]
fn validate_add_from_path() -> Result<()> {
    let project = Project::new("foo")?;

    project
        .wit(["add", "--path", "foo/baz", "foo:baz"])
        .assert()
        .stderr(contains("Added dependency `foo:baz` from path `foo/baz`"));

    let manifest = fs::read_to_string(project.root().join("wit.toml"))?;
    assert!(contains(r#""foo:baz" = { path = "foo/baz" }"#).eval(&manifest));

    Ok(())
}
