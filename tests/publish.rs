use std::fs;

use anyhow::{Context, Result};
use assert_cmd::prelude::*;
use predicates::str::contains;

use crate::support::*;

mod support;

#[test]
fn help() {
    for arg in ["help publish", "publish -h", "publish --help"] {
        cargo_component(arg.split_whitespace())
            .assert()
            .stdout(contains("Publish a package to a registry"))
            .success();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_component() -> Result<()> {
    let (server, config, _) = spawn_server(Vec::<String>::new()).await?;

    publish_wit(
        config,
        "test:world",
        "1.0.0",
        r#"package test:%world@1.0.0;
world foo {
    import foo: func() -> string;
    export bar: func() -> string;
}"#,
    )
    .await?;

    let project = server.project(
        "foo",
        true,
        ["--namespace", "test", "--target", "test:world"],
    )?;

    // Ensure there's a using declaration in the generated source
    let source = fs::read_to_string(project.root().join("src/lib.rs"))?;
    assert!(source.contains("use bindings::Guest;"));

    project
        .cargo_component(["publish"])
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    let path = project.root().join("Cargo-component.lock");
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read lock file `{path}`", path = path.display()))?;

    assert!(contents.contains("name = \"test:world\""));
    assert!(contents.contains("version = \"1.0.0\""));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_dependency() -> Result<()> {
    let (server, config, _) = spawn_server(Vec::<String>::new()).await?;

    publish_wit(
        config,
        "test:world",
        "1.0.0",
        r#"package test:%world@1.0.0;
world foo {
    export bar: func() -> string;
}"#,
    )
    .await?;

    let project = server.project(
        "foo",
        true,
        ["--namespace", "test", "--target", "test:world/foo"],
    )?;

    project
        .cargo_component(["publish"])
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    let project = server.project(
        "bar",
        true,
        ["--namespace", "test", "--target", "test:world"],
    )?;
    project
        .cargo_component(["add", "test:foo"])
        .assert()
        .stderr(contains("Added dependency `test:foo` with version `0.1.0`"))
        .success();

    let source = r#"
#[allow(warnings)]
mod bindings;
use bindings::Guest;
struct Component;
impl Guest for Component {
    fn bar() -> String {
        bindings::test_foo::bar()
    }
}

bindings::export!(Component with_types_in bindings);
"#;

    fs::write(project.root().join("src/lib.rs"), source)?;

    project
        .cargo_component(["publish"])
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("bar"))?;

    Ok(())
}
