use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;
use toml_edit::{value, Item, Table};

mod support;

#[test]
fn it_runs_with_basic_component() -> Result<()> {
    let project = Project::new("bar")?;
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
package my:command

world generator {
    export wasi:cli/run
}",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        r#"
cargo_component_bindings::generate!();

use bindings::exports::wasi::cli::run::Guest;

struct Component;

impl Guest for Component {
    fn run() -> Result<(), ()> {
        if std::env::args().any(|v| v == "--verbose") {
            println!("[guest] running component 'my:command'");
        }
        Ok(())
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
