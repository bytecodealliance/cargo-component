use std::fs;

use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::PredicateBooleanExt, str::contains, Predicate};

use crate::support::*;

mod support;

#[test]
fn help() {
    for arg in ["help update", "update -h", "update --help"] {
        wit(arg.split_whitespace())
            .assert()
            .stdout(contains("Update dependencies as recorded in the lock file"))
            .success();
    }
}

#[test]
fn it_fails_with_missing_toml_file() -> Result<()> {
    wit(["update"])
        .assert()
        .stderr(contains(
            "error: failed to find configuration file `wit.toml`",
        ))
        .failure();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_without_changes_is_a_noop() -> Result<()> {
    let (server, _, _) = spawn_server(Vec::<String>::new()).await?;

    let project = server.project("bar", Vec::<String>::new())?;
    project.file("bar.wit", "package test:bar;\n")?;
    project
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v0.1.0"))
        .success();

    let project = server.project("baz", Vec::<String>::new())?;
    project.file("baz.wit", "package test:baz;\n")?;
    project
        .wit(["add", "test:bar"])
        .assert()
        .stderr(contains("Added dependency `test:bar` with version `0.1.0"))
        .success();

    project
        .wit(["build"])
        .assert()
        .success()
        .stderr(contains("Created package `baz.wasm`"));

    project
        .wit(["update"])
        .assert()
        .success()
        .stderr(contains("test:bar").not());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_update_without_compatible_changes_is_a_noop() -> Result<()> {
    let (server, _, _) = spawn_server(Vec::<String>::new()).await?;

    let project1 = server.project("bar", Vec::<String>::new())?;
    project1.file("bar.wit", "package test:bar;\n")?;

    project1
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v0.1.0"))
        .success();

    let project2 = server.project("baz", Vec::<String>::new())?;
    project2.file("baz.wit", "package test:baz;\n")?;
    project2
        .wit(["add", "test:bar"])
        .assert()
        .stderr(contains("Added dependency `test:bar` with version `0.1.0"))
        .success();

    project2
        .wit(["build"])
        .assert()
        .success()
        .stderr(contains("Created package `baz.wasm`"));

    project1.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n[registries]\n",
    )?;

    project1
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v1.0.0"))
        .success();

    project2
        .wit(["update"])
        .assert()
        .success()
        .stderr(contains("test:bar").not());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_with_compatible_changes() -> Result<()> {
    let (server, _, _) = spawn_server(Vec::<String>::new()).await?;

    let project1 = server.project("bar", Vec::<String>::new())?;
    project1.file("bar.wit", "package test:bar;\n")?;
    project1.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n[registries]\n",
    )?;

    project1
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v1.0.0"))
        .success();

    let project2 = server.project("baz", Vec::<String>::new())?;
    project2.file("baz.wit", "package test:baz;\n")?;
    project2
        .wit(["add", "test:bar"])
        .assert()
        .stderr(contains("Added dependency `test:bar` with version `1.0.0"))
        .success();

    project2
        .wit(["build"])
        .assert()
        .success()
        .stderr(contains("Created package `baz.wasm`"));

    project1.file(
        "wit.toml",
        "version = \"1.1.0\"\n[dependencies]\n[registries]\n",
    )?;

    project1
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v1.1.0"))
        .success();

    project2
        .wit(["update"])
        .assert()
        .success()
        .stderr(contains("Updating dependency `test:bar` v1.0.0 -> v1.1.0"));

    let lock_file = fs::read_to_string(project2.root().join("wit.lock"))?;
    assert!(contains("version = \"1.1.0\"").eval(&lock_file));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_with_compatible_changes_is_noop_for_dryrun() -> Result<()> {
    let (server, _, _) = spawn_server(Vec::<String>::new()).await?;

    let project1 = server.project("bar", Vec::<String>::new())?;
    project1.file("bar.wit", "package test:bar;\n")?;
    project1.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n[registries]\n",
    )?;

    project1
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v1.0.0"))
        .success();

    let project2 = server.project("baz", Vec::<String>::new())?;
    project2.file("baz.wit", "package test:baz;\n")?;
    project2
        .wit(["add", "test:bar"])
        .assert()
        .stderr(contains("Added dependency `test:bar` with version `1.0.0"))
        .success();

    project2
        .wit(["build"])
        .assert()
        .success()
        .stderr(contains("Created package `baz.wasm`"));

    project1.file(
        "wit.toml",
        "version = \"1.1.0\"\n[dependencies]\n[registries]\n",
    )?;

    project1
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v1.1.0"))
        .success();

    project2
        .wit(["update", "--dry-run"])
        .assert()
        .success()
        .stderr(
            contains("Would update dependency `test:bar` v1.0.0 -> v1.1.0").and(contains(
                "warning: not updating lock file due to --dry-run option",
            )),
        );

    let lock_file = fs::read_to_string(project2.root().join("wit.lock"))?;
    assert!(contains("version = \"1.1.0\"").not().eval(&lock_file));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_with_changed_dependencies() -> Result<()> {
    let (server, _, _) = spawn_server(Vec::<String>::new()).await?;

    let project = server.project("bar", Vec::<String>::new())?;
    project.file("bar.wit", "package test:bar;\n")?;
    project.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n[registries]\n",
    )?;

    project
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v1.0.0"))
        .success();

    let project = server.project("baz", Vec::<String>::new())?;
    project.file("baz.wit", "package test:baz;\n")?;
    project.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n[registries]\n",
    )?;

    project
        .wit(["publish"])
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:baz` v1.0.0"))
        .success();

    let project = server.project("qux", Vec::<String>::new())?;
    project.file("qux.wit", "package test:qux;\n")?;
    project
        .wit(["add", "test:bar"])
        .assert()
        .stderr(contains("Added dependency `test:bar` with version `1.0.0"))
        .success();

    project
        .wit(["build"])
        .assert()
        .stderr(contains("Created package `qux.wasm`"))
        .success();

    project.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n\"test:baz\" = \"1.0.0\"\n[registries]\n",
    )?;

    project
        .wit(["update"])
        .assert()
        .stderr(
            contains("Removing dependency `test:bar` v1.0.0")
                .and(contains("Adding dependency `test:baz` v1.0.0")),
        )
        .success();

    project
        .wit(["build"])
        .assert()
        .stderr(contains("Created package `qux.wasm`"))
        .success();

    let path = project.root().join("qux.wasm");
    validate_component(&path)?;

    Ok(())
}
