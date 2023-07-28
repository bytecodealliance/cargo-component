use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use warg_client::FileSystemClient;

mod support;

#[test]
fn help() {
    for arg in ["help publish", "publish -h", "publish --help"] {
        wit(arg)
            .assert()
            .stdout(contains("Publish a WIT package to a registry"))
            .success();
    }
}

#[test]
fn it_fails_with_missing_toml_file() -> Result<()> {
    wit("publish")
        .assert()
        .stderr(contains(
            "error: failed to find configuration file `wit.toml`",
        ))
        .failure();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_wit_package() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    let project = Project::with_root(&root, "foo", "")?;
    project.file("baz.wit", "package baz:qux\n")?;
    project
        .wit("publish --init")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `baz:qux` v0.1.0"))
        .success();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_does_a_dry_run_publish() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    let project = Project::with_root(&root, "foo", "")?;
    project.file("baz.wit", "package baz:qux\n")?;
    project
        .wit("publish --init --dry-run")
        .env("WIT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains(
            "warning: not publishing package to the registry due to the --dry-run option",
        ))
        .success();

    let client = FileSystemClient::new_with_config(None, &config)?;

    assert!(client
        .download(&"baz:qux".parse().unwrap(), &"0.1.0".parse().unwrap())
        .await
        .unwrap_err()
        .to_string()
        .contains("package `baz:qux` does not exist"));

    Ok(())
}
