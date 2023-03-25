use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;

mod support;

#[test]
fn help() {
    for arg in ["help publish", "publish -h", "publish --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains("Publish a package to a registry"))
            .success();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_component() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    publish_wit(
        &config,
        "world",
        "1.0.0",
        r#"default world foo {
    import foo: func() -> string
    export bar: func() -> string
}"#,
        true,
    )
    .await?;

    let project = Project::with_root(&root, "foo", "--target world")?;

    project
        .cargo_component("publish --name foo --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_fails_if_package_does_not_exist() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    publish_wit(
        &config,
        "world",
        "1.0.0",
        r#"default world foo {
    import foo: func() -> string
    export bar: func() -> string
}"#,
        true,
    )
    .await?;

    let project = Project::with_root(&root, "foo", "--target world")?;

    project
        .cargo_component("publish --name foo")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("error: package `foo` was not found"))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_dependency() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    publish_wit(
        &config,
        "world",
        "1.0.0",
        r#"default world foo {
    export bar: func() -> string
}"#,
        true,
    )
    .await?;

    let project = Project::with_root(&root, "foo", "--target world")?;

    project
        .cargo_component("publish --name test/foo --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test/foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    let project = Project::with_root(&root, "bar", "--target world")?;

    project
        .cargo_component("add foo test/foo")
        .assert()
        .stderr(contains("Added dependency `foo` with version `0.1.0`"))
        .success();

    let source = r#"use bindings::Foo;
struct Component;
impl Foo for Component {
    fn bar() -> String {
        bindings::foo::bar()
    }
}
bindings::export!(Component);
"#;

    fs::write(project.root().join("src/lib.rs"), source)?;

    project
        .cargo_component("publish --name test/bar --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test/bar` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("bar"))?;

    Ok(())
}
