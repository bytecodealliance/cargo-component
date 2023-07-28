use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::PredicateBooleanExt, str::contains};

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
        .build();

    project
        .cargo_component("new --reactor foo")
        .assert()
        .stderr(contains("Updated manifest of package `foo`"))
        .success();

    let member = ProjectBuilder::new(project.root().join("foo")).build();
    member.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    project
        .cargo_component("new --reactor bar")
        .assert()
        .stderr(contains("Updated manifest of package `bar`"))
        .success();

    let member = ProjectBuilder::new(project.root().join("bar")).build();
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
