use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;

mod support;

#[test]
fn help() {
    for arg in ["help build", "build -h", "build --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Compile a WebAssembly component and all of its dependencies",
            ))
            .success();
    }
}

#[test]
fn it_builds_debug() -> Result<()> {
    let project = Project::new("foo")?;
    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();

    validate_component(&project.debug_wasm("foo"))?;

    Ok(())
}

#[test]
fn it_builds_release() -> Result<()> {
    let project = Project::new("foo")?;
    project
        .cargo_component("build --release")
        .assert()
        .stderr(contains("Finished release [optimized] target(s)"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    Ok(())
}

#[test]
fn it_builds_a_workspace() -> Result<()> {
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
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();

    validate_component(&project.debug_wasm("foo"))?;
    validate_component(&project.debug_wasm("bar"))?;

    Ok(())
}
