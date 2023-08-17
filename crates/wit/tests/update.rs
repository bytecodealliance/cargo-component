use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::PredicateBooleanExt, str::contains, Predicate};
use std::fs;

mod support;

#[test]
fn help() {
    for arg in ["help update", "update -h", "update --help"] {
        wit(arg)
            .assert()
            .stdout(contains("Update dependencies as recorded in the lock file"))
            .success();
    }
}

#[test]
fn it_fails_with_missing_toml_file() -> Result<()> {
    wit("update")
        .assert()
        .stderr(contains(
            "error: failed to find configuration file `wit.toml`",
        ))
        .failure();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_without_changes_is_a_noop() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    let project = Project::with_root(&root, "bar", "")?;
    project.file("bar.wit", "package foo:bar\n")?;
    project
        .wit("publish --init")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `foo:bar` v0.1.0"))
        .success();

    let project = Project::with_root(&root, "baz", "")?;
    project.file("baz.wit", "package foo:baz\n")?;
    project
        .wit("add foo:bar")
        .assert()
        .stderr(contains("Added dependency `foo:bar` with version `0.1.0"))
        .success();

    project
        .wit("update")
        .assert()
        .success()
        .stderr(contains("foo:bar").not());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_update_without_compatible_changes_is_a_noop() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    let project = Project::with_root(&root, "bar", "")?;
    project.file("bar.wit", "package foo:bar\n")?;
    project
        .wit("publish --init")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `foo:bar` v0.1.0"))
        .success();

    let project = Project::with_root(&root, "baz", "")?;
    project.file("baz.wit", "package foo:baz\n")?;
    project
        .wit("add foo:bar")
        .assert()
        .stderr(contains("Added dependency `foo:bar` with version `0.1.0"))
        .success();

    fs::write(
        root.join("bar/wit.toml"),
        "version = \"1.0.0\"\n[dependencies]\n[registries]\n",
    )?;

    wit("publish")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .current_dir(root.join("bar"))
        .assert()
        .stderr(contains("Published package `foo:bar` v1.0.0"))
        .success();

    project
        .wit("update")
        .assert()
        .success()
        .stderr(contains("foo:bar").not());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_with_compatible_changes() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    let project = Project::with_root(&root, "bar", "")?;
    project.file("bar.wit", "package foo:bar\n")?;
    project.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n[registries]\n",
    )?;

    project
        .wit("publish --init")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `foo:bar` v1.0.0"))
        .success();

    let project = Project::with_root(&root, "baz", "")?;
    project.file("baz.wit", "package foo:baz\n")?;
    project
        .wit("add foo:bar")
        .assert()
        .stderr(contains("Added dependency `foo:bar` with version `1.0.0"))
        .success();

    fs::write(
        root.join("bar/wit.toml"),
        "version = \"1.1.0\"\n[dependencies]\n[registries]\n",
    )?;

    wit("publish")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .current_dir(root.join("bar"))
        .assert()
        .stderr(contains("Published package `foo:bar` v1.1.0"))
        .success();

    project
        .wit("update")
        .assert()
        .success()
        .stderr(contains("Updating dependency `foo:bar` v1.0.0 -> v1.1.0"));

    let lock_file = fs::read_to_string(project.root().join("wit.lock"))?;
    assert!(contains("version = \"1.1.0\"").eval(&lock_file));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_with_compatible_changes_is_noop_for_dryrun() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    let project = Project::with_root(&root, "bar", "")?;
    project.file("bar.wit", "package foo:bar\n")?;
    project.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n[registries]\n",
    )?;

    project
        .wit("publish --init")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `foo:bar` v1.0.0"))
        .success();

    let project = Project::with_root(&root, "baz", "")?;
    project.file("baz.wit", "package foo:baz\n")?;
    project
        .wit("add foo:bar")
        .assert()
        .stderr(contains("Added dependency `foo:bar` with version `1.0.0"))
        .success();

    fs::write(
        root.join("bar/wit.toml"),
        "version = \"1.1.0\"\n[dependencies]\n[registries]\n",
    )?;

    wit("publish")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .current_dir(root.join("bar"))
        .assert()
        .stderr(contains("Published package `foo:bar` v1.1.0"))
        .success();

    project.wit("update --dry-run").assert().success().stderr(
        contains("Would update dependency `foo:bar` v1.0.0 -> v1.1.0").and(contains(
            "warning: not updating lock file due to --dry-run option",
        )),
    );

    let lock_file = fs::read_to_string(project.root().join("wit.lock"))?;
    assert!(contains("version = \"1.1.0\"").not().eval(&lock_file));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_with_changed_dependencies() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    let project = Project::with_root(&root, "bar", "")?;
    project.file("bar.wit", "package foo:bar\n")?;
    project.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n[registries]\n",
    )?;

    project
        .wit("publish --init")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `foo:bar` v1.0.0"))
        .success();

    let project = Project::with_root(&root, "baz", "")?;
    project.file("baz.wit", "package foo:baz\n")?;
    project.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n[registries]\n",
    )?;

    project
        .wit("publish --init")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `foo:baz` v1.0.0"))
        .success();

    let project = Project::with_root(&root, "qux", "")?;
    project.file("qux.wit", "package foo:qux\n")?;
    project
        .wit("add foo:bar")
        .assert()
        .stderr(contains("Added dependency `foo:bar` with version `1.0.0"))
        .success();

    project
        .wit("build")
        .assert()
        .stderr(contains("Created package `qux.wasm`"))
        .success();

    project.file(
        "wit.toml",
        "version = \"1.0.0\"\n[dependencies]\n\"foo:baz\" = \"1.0.0\"\n[registries]\n",
    )?;

    project
        .wit("update")
        .assert()
        .stderr(
            contains("Removing dependency `foo:bar` v1.0.0")
                .and(contains("Adding dependency `foo:baz` v1.0.0")),
        )
        .success();

    project
        .wit("build")
        .assert()
        .stderr(contains("Created package `qux.wasm`"))
        .success();

    let path = project.root().join("qux.wasm");
    validate_component(&path)?;

    Ok(())
}
