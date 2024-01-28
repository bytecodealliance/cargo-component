use std::rc::Rc;

use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::PredicateBooleanExt, str::contains};
use tempfile::TempDir;

mod support;

#[test]
fn it_prints_metadata() -> Result<()> {
    let project = Project::new("foo")?;

    project
        .cargo_component("metadata --format-version 1")
        .assert()
        .stdout(contains(r#""name":"foo","version":"0.1.0""#))
        .success();

    Ok(())
}

#[test]
fn it_rejects_invalid_format_versions() -> Result<()> {
    let project = Project::new("foo")?;

    for arg in ["7", "0", "42"] {
        project
            .cargo_component(&format!("metadata --format-version {arg}"))
            .assert()
            .stderr(contains("invalid value"))
            .failure();
    }

    Ok(())
}

#[test]
fn it_prints_workspace_metadata() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let project = Project {
        dir: dir.clone(),
        root: dir.path().to_owned(),
    };

    project.file(
        "baz/Cargo.toml",
        r#"[package]
name = "baz"
version = "0.1.0"
edition = "2021"
    
[dependencies]
"#,
    )?;

    project.file("baz/src/lib.rs", "")?;

    project
        .cargo_component("new --lib foo")
        .assert()
        .stderr(contains("Updated manifest of package `foo`"))
        .success();

    project
        .cargo_component("new --lib bar")
        .assert()
        .stderr(contains("Updated manifest of package `bar`"))
        .success();

    // Add the workspace after all of the packages have been created.
    project.file(
        "Cargo.toml",
        r#"[workspace]
members = ["foo", "bar", "baz"]
"#,
    )?;

    project
        .cargo_component("metadata --format-version 1")
        .assert()
        .stdout(
            contains(r#"name":"foo","version":"0.1.0""#).and(
                contains(r#"name":"bar","version":"0.1.0""#)
                    .and(contains(r#"name":"baz","version":"0.1.0""#)),
            ),
        )
        .success();

    Ok(())
}
