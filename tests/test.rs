use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;
use toml_edit::{value, Item, Table};

mod support;

#[test]
fn it_runs_test_with_command_component() -> Result<()> {
    let project = Project::new_bin("foo-bar")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    fs::create_dir_all(project.root().join(".cargo"))?;
    fs::write(
        project.root().join(".cargo/config.toml"),
        r#"
[target.wasm32-wasi]
runner = [
    "wasmtime",
    "-C",
    "cache=no",
    "-W",
    "component-model",
    "-S",
    "preview2",
    "-S",
    "common",
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
    export rand: func(seed: seed) -> u32;
    export wasi:cli/run;
}",
    )?;

    fs::write(
        project.root().join("src/main.rs"),
        r#"
cargo_component_bindings::generate!();

use bindings::{Seed};

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
        .stdout(contains("test test_random_component ..."))
        .stdout(contains("test result: FAILED."))
        .success();

    Ok(())
}

#[test]
fn it_runs_test_with_reactor_component() -> Result<()> {
    let project = Project::new("foo-bar")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        let mut dependencies = Table::new();
        dependencies["wasi:cli"]["path"] = value("wit/deps/cli");

        let target =
            doc["package"]["metadata"]["component"]["target"].or_insert(Item::Table(Table::new()));
        target["dependencies"] = Item::Table(dependencies);
        Ok(doc)
    })?;

    fs::create_dir_all(project.root().join("wit/deps/cli"))?;
    fs::write(
        project.root().join("wit/deps/cli/run.wit"),
        "
    package wasi:cli

    interface run {
        run: func() -> result
    }",
    )?;

    fs::write(
        project.root().join("wit/world.wit"),
        "
package my:random

interface types {
    record seed {
        value: u32,
    }
}

world generator {
    use types.{seed}
    import get-seed: func() -> seed
    export wasi:cli/run
}",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        r#"
cargo_component_bindings::generate!();

use bindings::exports::wasi::cli::run::Guest as Run;
use bindings::{Seed};

struct Component;

fn rand(seed: Seed) -> u32 {
    seed.value + 1
}

impl Run for Component {
    fn run() -> Result<(), ()> {
        Ok(())
    }
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
        .stdout(contains("test test_random_component ..."))
        .stdout(contains("test result: ok."))
        .success();

    Ok(())
}
