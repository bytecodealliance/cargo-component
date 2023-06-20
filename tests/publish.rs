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
        "my:world",
        "1.0.0",
        r#"package my:%world@1.0.0
world foo {
    import foo: func() -> string
    export bar: func() -> string
}"#,
        true,
    )
    .await?;

    let project = Project::with_root(&root, "foo", "--target my:world")?;

    // Ensure there's a using declaration in the generated source
    let source = fs::read_to_string(project.root().join("src/lib.rs"))?;
    assert!(source.contains("use bindings::Foo;"));

    project
        .cargo_component("publish --id test:foo --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:foo` v0.1.0"))
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
        "my:world",
        "1.0.0",
        r#"package my:%world@1.0.0
world foo {
    import foo: func() -> string
    export bar: func() -> string
}"#,
        true,
    )
    .await?;

    let project = Project::with_root(&root, "foo", "--target my:world")?;

    project
        .cargo_component("publish --id test:foo")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("error: package `test:foo` does not exist"))
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
        "my:world",
        "1.0.0",
        r#"package my:%world@1.0.0 
world foo {
    export bar: func() -> string
}"#,
        true,
    )
    .await?;

    let project = Project::with_root(&root, "foo", "--target my:world/foo")?;

    project
        .cargo_component("publish --id test:foo --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    let project = Project::with_root(&root, "bar", "--target my:world")?;

    project
        .cargo_component("add test:foo")
        .assert()
        .stderr(contains("Added dependency `test:foo` with version `0.1.0`"))
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
        .cargo_component("publish --id test:bar --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("bar"))?;

    Ok(())
}
