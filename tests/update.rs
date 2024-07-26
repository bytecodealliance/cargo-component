use crate::support::*;
use anyhow::{Context, Result};
use assert_cmd::prelude::*;
use predicates::{prelude::PredicateBooleanExt, str::contains};
use std::{fs, rc::Rc};
use tempfile::TempDir;
use toml_edit::value;

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
async fn update_without_changes_is_a_noop() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    publish_wit(
        &config,
        "test:bar",
        "1.0.0",
        r#"package test:bar@1.0.0;
world foo {
    import foo: func() -> string;
    export bar: func() -> string;
}"#,
        true,
    )
    .await?;

    let project = Project::with_dir(dir.clone(), "component", "--target test:bar@1.0.0")?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    project
        .cargo_component("update")
        .assert()
        .success()
        .stderr(contains("test:bar").not());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_without_compatible_changes_is_a_noop() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    publish_wit(
        &config,
        "test:bar",
        "1.0.0",
        r#"package test:bar@1.0.0;
world foo {
    import foo: func() -> string;
    export bar: func() -> string;
}"#,
        true,
    )
    .await?;

    let project = Project::with_dir(dir.clone(), "component", "--target test:bar@1.0.0")?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    publish_wit(
        &config,
        "test:bar",
        "2.0.0",
        r#"package test:bar@2.0.0;
world foo {
    export bar: func() -> string;
}"#,
        false,
    )
    .await?;

    project
        .cargo_component("update")
        .assert()
        .success()
        .stderr(contains("test:bar").not());

    project
        .cargo_component("build")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_with_compatible_changes() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    publish_wit(
        &config,
        "test:bar",
        "1.0.0",
        r#"package test:bar@1.0.0;
world foo {
    import foo: func() -> string;
    export bar: func() -> string;
}"#,
        true,
    )
    .await?;

    let project = Project::with_dir(dir.clone(), "component", "--target test:bar@1.0.0")?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    publish_wit(
        &config,
        "test:bar",
        "1.1.0",
        r#"package test:bar@1.1.0;
world foo {
    import foo: func() -> string;
    import baz: func() -> string;
    export bar: func() -> string;
}"#,
        false,
    )
    .await?;

    project
        .cargo_component("update")
        .assert()
        .success()
        .stderr(contains("`test:bar` v1.0.0 -> v1.1.0"));

    let source = r#"
#[allow(warnings)]
mod generated;
use generated::{baz, Guest};
struct Component;
impl Guest for Component {
    fn bar() -> String {
        baz()
    }
}

generated::export!(Component with_types_in generated);
"#;

    fs::write(project.root().join("src/lib.rs"), source)?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_with_compatible_changes_is_noop_for_dryrun() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    publish_wit(
        &config,
        "test:bar",
        "1.0.0",
        r#"package test:bar@1.0.0;
world foo {
    import foo: func() -> string;
    export bar: func() -> string;
}"#,
        true,
    )
    .await?;

    let project = Project::with_dir(dir.clone(), "component", "--target test:bar@1.0.0")?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    publish_wit(
        &config,
        "test:bar",
        "1.1.0",
        r#"package test:bar@1.1.0;
world foo {
    import foo: func() -> string;
    import baz: func() -> string;
    export bar: func() -> string;
}"#,
        false,
    )
    .await?;

    project
        .cargo_component("update --dry-run")
        .assert()
        .success()
        .stderr(contains(
            "Would update dependency `test:bar` v1.0.0 -> v1.1.0",
        ));

    project
        .cargo_component("build")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(&project.debug_wasm("component"))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_with_changed_dependencies() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    publish_component(&config, "test:bar", "1.0.0", "(component)", true).await?;
    publish_component(&config, "test:baz", "1.0.0", "(component)", true).await?;

    let project = Project::with_dir(dir.clone(), "foo", "")?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(&project.debug_wasm("foo"))?;

    project
        .cargo_component("add test:bar")
        .assert()
        .stderr(contains("Added dependency `test:bar` with version `1.0.0`"))
        .success();

    project
        .cargo_component("build")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    project.update_manifest(|mut doc| {
        let deps = doc["package"]["metadata"]["component"]["dependencies"]
            .as_table_mut()
            .context("missing deps table")?;
        deps.remove("test:bar").context("missing dependency")?;
        deps.insert("test:baz", value("1.0.0"));
        Ok(doc)
    })?;

    project
        .cargo_component("update")
        .assert()
        .stderr(
            contains("Removing dependency `test:bar` v1.0.0")
                .and(contains("Adding dependency `test:baz` v1.0.0")),
        )
        .success();

    project
        .cargo_component("build")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    Ok(())
}
