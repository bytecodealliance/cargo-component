use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;

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
        .stdout(contains("interface 0.1.0"))
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
