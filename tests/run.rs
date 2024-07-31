use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;
use toml_edit::{value, Item, Table};

mod support;

#[test]
fn it_runs_with_command_component() -> Result<()> {
    let project = Project::new_bin("bar")?;

    fs::write(
        project.root().join("src/main.rs"),
        r#"
fn main() {
    if std::env::args().any(|v| v == "--verbose") {
        println!("[guest] running component 'my:command'");
    }
}"#,
    )?;

    project
        .cargo_component("run")
        .arg("--")
        .arg("--verbose")
        .assert()
        .stdout(contains("[guest] running component 'my:command'"))
        .success();

    validate_component(&project.debug_wasm("bar"))?;

    Ok(())
}

#[test]
fn it_runs_with_reactor_component() -> Result<()> {
    let project = Project::new("baz")?;
    project.update_manifest(|mut doc| {
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
package wasi:cli@0.2.0;

interface run {
    run: func() -> result;
}",
    )?;

    fs::write(
        project.root().join("wit/world.wit"),
        "
package my:reactor;

world generator {
    export wasi:cli/run@0.2.0;
}",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        r#"
#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::cli::run::Guest;

struct Component;

impl Guest for Component {
    fn run() -> Result<(), ()> {
        println!("[guest] running component 'my:reactor'");
        match std::env::vars().find_map(|(k, v)| if k == "APP_NAME" { Some(v) } else { None }) {
            Some(value) => println!("Hello, {}!", value),
            None => println!("Hello, World!"),
        }
        Ok(())
    }
}

bindings::export!(Component with_types_in bindings);
"#,
    )?;

    project
        .cargo_component("run")
        .env(
            "CARGO_TARGET_WASM32_WASIP1_RUNNER",
            "wasmtime --env APP_NAME=CargoComponent -C cache=no -W component-model -S preview2 -S cli",
        )
        .assert()
        .stdout(contains("[guest] running component 'my:reactor'"))
        .stdout(contains("Hello, CargoComponent!"))
        .success();

    validate_component(&project.debug_wasm("baz"))?;

    Ok(())
}
