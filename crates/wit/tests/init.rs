use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;

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
    let root = create_root()?;

    wit("init foo")
        .current_dir(&root)
        .assert()
        .stderr(contains("Created configuration file `foo/wit.toml`"))
        .success();

    let proj_dir = root.join("foo");
    assert!(proj_dir.join("wit.toml").is_file());

    Ok(())
}

#[test]
fn it_supports_registry_option() -> Result<()> {
    let root = create_root()?;

    wit("init bar --registry https://example.com")
        .current_dir(&root)
        .assert()
        .stderr(contains("Created configuration file `bar/wit.toml`"))
        .success();

    let proj_dir = root.join("bar");
    assert!(fs::read_to_string(proj_dir.join("wit.toml"))?
        .contains("default = \"https://example.com/\""));

    Ok(())
}
