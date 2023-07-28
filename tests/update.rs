use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::PredicateBooleanExt, str::contains};
use std::fs;

mod support;

#[test]
fn help() {
    for arg in ["help update", "update -h", "update --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Update dependencies as recorded in the component lock file",
            ))
            .success();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_update_without_changes_is_a_noop() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    publish_wit(
        &config,
        "foo:bar",
        "1.0.0",
        r#"package foo:bar@1.0.0
world foo {
    import foo: func() -> string
    export bar: func() -> string
}"#,
        true,
    )
    .await?;

    let project = Project::with_root(&root, "component", "--target foo:bar@1.0.0")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    project
        .cargo_component("update")
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

    publish_wit(
        &config,
        "foo:bar",
        "1.0.0",
        r#"package foo:bar@1.0.0
world foo {
    import foo: func() -> string
    export bar: func() -> string
}"#,
        true,
    )
    .await?;

    let project = Project::with_root(&root, "component", "--target foo:bar@1.0.0")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    publish_wit(
        &config,
        "foo:bar",
        "2.0.0",
        r#"package foo:bar@2.0.0
world foo {
    export bar: func() -> string
}"#,
        false,
    )
    .await?;

    project
        .cargo_component("update")
        .assert()
        .success()
        .stderr(contains("foo:bar").not());

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_update_with_compatible_changes() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    publish_wit(
        &config,
        "foo:bar",
        "1.0.0",
        r#"package foo:bar@1.0.0
world foo {
    import foo: func() -> string
    export bar: func() -> string
}"#,
        true,
    )
    .await?;

    let project = Project::with_root(&root, "component", "--target foo:bar@1.0.0")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    publish_wit(
        &config,
        "foo:bar",
        "1.1.0",
        r#"package foo:bar@1.1.0
world foo {
    import foo: func() -> string
    import baz: func() -> string
    export bar: func() -> string
}"#,
        false,
    )
    .await?;

    project
        .cargo_component("update")
        .assert()
        .success()
        .stderr(contains("`foo:bar` v1.0.0 -> v1.1.0"));

    let source = r#"cargo_component_bindings::generate!();
use bindings::{baz, Foo};
struct Component;
impl Foo for Component {
    fn bar() -> String {
        baz()
    }
}
"#;

    fs::write(project.root().join("src/lib.rs"), source)?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    Ok(())
}
