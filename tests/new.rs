use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;

mod support;

#[test]
fn help() {
    for arg in ["help new", "new -h", "new --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Create a new WebAssembly component package at <path>",
            ))
            .success();
    }
}

#[test]
fn it_creates_the_expected_files() -> Result<()> {
    let root = create_root()?;

    cargo_component("new foo")
        .current_dir(&root)
        .assert()
        .stderr(contains("Created component `foo` package"))
        .success();

    let proj_dir = root.join("foo");

    assert!(proj_dir.join("Cargo.toml").is_file());
    assert!(proj_dir.join("world.wit").is_file());
    assert!(proj_dir.join("src").join("lib.rs").is_file());
    assert!(proj_dir.join(".vscode").join("settings.json").is_file());

    Ok(())
}

#[test]
fn it_supports_editor_option() -> Result<()> {
    let root = create_root()?;

    cargo_component("new foo --editor none")
        .current_dir(&root)
        .assert()
        .stderr(contains("Created component `foo` package"))
        .success();

    let proj_dir = root.join("foo");

    assert!(proj_dir.join("Cargo.toml").is_file());
    assert!(proj_dir.join("world.wit").is_file());
    assert!(proj_dir.join("src").join("lib.rs").is_file());
    assert!(!proj_dir.join(".vscode").is_dir());

    Ok(())
}

#[test]
fn it_supports_edition_option() -> Result<()> {
    let root = create_root()?;

    cargo_component("new foo --edition 2018")
        .current_dir(&root)
        .assert()
        .stderr(contains("Created component `foo` package"))
        .success();

    let proj_dir = root.join("foo");

    assert!(fs::read_to_string(proj_dir.join("Cargo.toml"))?.contains("edition = \"2018\""));

    Ok(())
}

#[test]
fn it_supports_name_option() -> Result<()> {
    let root = create_root()?;

    cargo_component("new foo --name bar")
        .current_dir(&root)
        .assert()
        .stderr(contains("Created component `bar` package"))
        .success();

    let proj_dir = root.join("foo");

    assert!(fs::read_to_string(proj_dir.join("Cargo.toml"))?.contains("name = \"bar\""));

    Ok(())
}

#[test]
fn it_rejects_rust_keywords() -> Result<()> {
    let root = create_root()?;

    cargo_component("new foo --name fn")
        .current_dir(&root)
        .assert()
        .stderr(contains(
            "component name `fn` cannot be used as it is a Rust keyword",
        ))
        .failure();

    Ok(())
}
