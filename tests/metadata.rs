use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::PredicateBooleanExt, str::contains};

mod support;

#[test]
fn help() {
    for arg in ["help metadata", "metadata -h", "metadata --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains("Output the resolved dependencies of a package"))
            .success();
    }
}

#[test]
fn it_prints_metadata() -> Result<()> {
    let project = Project::new("foo")?;

    project
        .cargo_component("metadata --format-version 1")
        .assert()
        .stdout(contains("foo-interface 0.1.0"))
        .success();

    Ok(())
}

#[test]
fn it_rejects_invalid_format_versions() -> Result<()> {
    let project = Project::new("foo")?;

    for arg in ["bad", "1.42", "0", "42"] {
        project
            .cargo_component(format!("metadata --format-version {}", arg).as_str())
            .assert()
            .stderr(contains("Invalid value"))
            .failure();
    }

    Ok(())
}

#[test]
fn it_prints_workspace_metadata() -> Result<()> {
    let project = project()?
        .file(
            "Cargo.toml",
            r#"[workspace]
members = ["foo", "bar", "baz"]
"#,
        )?
        .file(
            "baz/Cargo.toml",
            r#"[package]
name = "baz"
version = "0.1.0"
edition = "2021"
    
[dependencies]
"#,
        )?
        .file("baz/src/lib.rs", "")?
        .build()?;

    project
        .cargo_component("new foo")
        .assert()
        .stderr(contains("Created component `foo` package"))
        .success();

    project
        .cargo_component("new bar")
        .assert()
        .stderr(contains("Created component `bar` package"))
        .success();

    project
        .cargo_component("metadata --format-version 1")
        .assert()
        .stdout(
            contains("foo-interface 0.1.0")
                .and(contains("bar-interface 0.1.0").and(contains("baz 0.1.0"))),
        )
        .success();

    Ok(())
}
