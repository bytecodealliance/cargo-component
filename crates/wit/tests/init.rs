use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::{fs, path::Path};
use tempfile::TempDir;

mod support;

#[test]
fn help() {
    for arg in ["help init", "init -h", "init --help"] {
        wit(arg)
            .assert()
            .stdout(contains("Initialize a new WIT package"))
            .success();
    }
}

#[test]
fn it_creates_the_expected_files() -> Result<()> {
    let dir = TempDir::new()?;

    wit("init foo")
        .current_dir(dir.path())
        .assert()
        .stderr(contains(format!(
            "Created configuration file `{path}`",
            path = Path::new("foo").join("wit.toml").display()
        )))
        .success();

    let proj_dir = dir.path().join("foo");
    assert!(proj_dir.join("wit.toml").is_file());

    Ok(())
}

#[test]
fn it_supports_registry_option() -> Result<()> {
    let dir = TempDir::new()?;

    wit("init bar --registry https://example.com")
        .current_dir(dir.path())
        .assert()
        .stderr(contains(format!(
            "Created configuration file `{path}`",
            path = Path::new("bar").join("wit.toml").display()
        )))
        .success();

    let proj_dir = dir.path().join("bar");
    assert!(fs::read_to_string(proj_dir.join("wit.toml"))?
        .contains("default = \"https://example.com/\""));

    Ok(())
}
