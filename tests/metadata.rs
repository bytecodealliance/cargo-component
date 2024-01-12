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
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    project
        .cargo_component("metadata --format-version 1")
        .assert()
        .stdout(contains("foo 0.1.0"))
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
        "Cargo.toml",
        r#"[workspace]
members = ["foo", "bar", "baz"]
"#,
    )?;

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
        .cargo_component("new --reactor foo")
        .assert()
        .stderr(contains("Updated manifest of package `foo`"))
        .success();

    let member = Project {
        dir: dir.clone(),
        root: project.root().join("foo"),
    };
    member.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    project
        .cargo_component("new --reactor bar")
        .assert()
        .stderr(contains("Updated manifest of package `bar`"))
        .success();

    let member = Project {
        dir: dir.clone(),
        root: project.root().join("bar"),
    };
    member.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    project
        .cargo_component("metadata --format-version 1")
        .assert()
        .stdout(contains("foo 0.1.0").and(contains("bar 0.1.0").and(contains("baz 0.1.0"))))
        .success();

    Ok(())
}
