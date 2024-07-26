use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;

mod support;

#[test]
fn it_runs_test_with_command_component() -> Result<()> {
    let project = Project::new_bin("foo-bar")?;

    fs::create_dir_all(project.root().join(".cargo"))?;
    fs::write(
        project.root().join(".cargo/config.toml"),
        r#"
[target.wasm32-wasip1]
runner = [
    "wasmtime",
    "-C",
    "cache=no",
    "-W",
    "component-model",
    "-S",
    "preview2",
    "-S",
    "cli",
]"#,
    )?;

    fs::create_dir_all(project.root().join("wit"))?;
    fs::write(
        project.root().join("wit/world.wit"),
        "
package my:random;

interface types {
    record seed {
        value: u32,
    }
}

world generator {
    use types.{seed};
    import get-seed: func() -> seed;
}",
    )?;

    fs::write(
        project.root().join("src/main.rs"),
        r#"
#[allow(warnings)]
mod generated;

use generated::{Seed};

fn rand(seed: Seed) -> u32 {
    seed.value + 1
}

fn main() {
    println!("");
}

#[test]
pub fn test_random_component() {
    let result = rand(Seed { value: 3 });
    assert_eq!(result, 4);
}"#,
    )?;

    project
        .cargo_component("test")
        .assert()
        .stdout(contains("test test_random_component ... ok"))
        .stdout(contains("test result: ok."))
        .success();

    Ok(())
}

#[test]
fn it_runs_test_with_reactor_component() -> Result<()> {
    let project = Project::new("foo-bar")?;

    fs::write(
        project.root().join("wit/world.wit"),
        "
package my:random;

interface types {
    record seed {
        value: u32,
    }
}

world generator {
    use types.{seed};
    import get-seed: func() -> seed;
}",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        r#"
#[allow(warnings)]
mod generated;

use generated::{Seed};

fn rand(seed: Seed) -> u32 {
    seed.value + 1
}

#[test]
pub fn test_random_component() {
    let result = rand(Seed { value: 6 });
    assert_eq!(result, 7);
}"#,
    )?;

    project
        .cargo_component("test")
        .assert()
        .stdout(contains("test test_random_component ... ok"))
        .stdout(contains("test result: ok."))
        .success();

    Ok(())
}
