use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{boolean::PredicateBooleanExt, str::contains};
use std::{fmt::Write, fs};

mod support;

#[test]
fn it_checks_a_new_project() -> Result<()> {
    let project = Project::new("foo")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    project
        .cargo_component("clippy")
        .assert()
        .stderr(contains("Checking foo v0.1.0"))
        .success();

    Ok(())
}

#[test]
fn it_finds_errors() -> Result<()> {
    let project = Project::new("foo")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    let mut src = fs::read_to_string(project.root().join("src/lib.rs"))?;
    write!(&mut src, "\n\nfn foo() -> String {{\n  \"foo\"\n}}\n")?;

    fs::write(project.root().join("src/lib.rs"), src)?;

    project
        .cargo_component("clippy")
        .assert()
        .stderr(contains("Checking foo v0.1.0").and(contains("expected `String`, found `&str`")))
        .failure();

    Ok(())
}

#[test]
fn it_finds_clippy_warnings() -> Result<()> {
    let project = Project::new("foo")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    let mut src = fs::read_to_string(project.root().join("src/lib.rs"))?;
    write!(
        &mut src,
        "\n\nfn foo() -> String {{\n  return \"foo\".to_string();\n}}\n"
    )?;

    fs::write(project.root().join("src/lib.rs"), src)?;

    project
        .cargo_component("clippy")
        .assert()
        .stderr(contains("Checking foo v0.1.0").and(contains("clippy::needless_return")))
        .success();

    Ok(())
}

#[test]
fn it_checks_a_workspace() -> Result<()> {
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
        .cargo_component("clippy")
        .assert()
        .stderr(contains("Checking foo v0.1.0").and(contains("Checking bar v0.1.0")))
        .success();

    Ok(())
}
