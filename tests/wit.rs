use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;
use toml_edit::{value, Document, InlineTable, Value};

mod support;

#[test]
fn help() {
    for arg in ["help wit", "wit -h", "wit --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains("Manages the target WIT package"))
            .success();
    }

    for arg in ["help wit publish", "wit publish -h", "wit publish --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains("Publishes the target WIT package to a registry"))
            .success();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_wit_package() -> Result<()> {
    let root = create_root()?;
    let (_server, config) = spawn_server(&root).await?;
    config.write_to_file(&root.join("warg-config.json"))?;

    let project = Project::with_root(&root, "foo", "")?;

    let wit = r#"default interface bar {
    baz: func() -> string
}

default world foo {}
"#;

    fs::write(project.root().join("wit/world.wit"), wit)?;

    project
        .cargo_component("wit publish foo --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `foo` v0.1.0"))
        .success();

    let project = Project::with_root(&root, "bar", "")?;

    let wit = r#"default world bar {
    import bar: foo.%world.bar
}
"#;

    fs::write(project.root().join("wit/world.wit"), wit)?;

    let manifest_path = project.root().join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)?;
    let mut doc: Document = manifest.parse()?;
    doc["package"]["metadata"]["component"]["target"] = value(InlineTable::from_iter(
        [
            ("path", Value::from("wit/world.wit")),
            (
                "dependencies",
                Value::from(InlineTable::from_iter(
                    [("foo", Value::from("foo@0.1.0"))].into_iter(),
                )),
            ),
        ]
        .into_iter(),
    ));
    fs::write(manifest_path, doc.to_string())?;

    project
        .cargo_component("wit publish bar --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `bar` v0.1.0"))
        .success();

    Ok(())
}
