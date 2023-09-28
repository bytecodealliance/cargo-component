use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;

mod support;

#[test]
fn it_runs_test_with_basic_component() -> Result<()> {
    let project = Project::new("foo-bar")?;
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
    export rand: func(seed: seed) -> u32
    export wasi:cli/run
}",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        r#"
cargo_component_bindings::generate!();

use bindings::{Guest, Seed};

struct Component;

impl Guest for Component {
    fn rand(seed: Seed) -> u32 {
        seed.value + 1
    }
}

#[test]
pub fn test_random_component() {
    let result = Component::rand(Seed { value: 3 });
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
