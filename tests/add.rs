use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::*, str::contains};
use std::fs;

mod support;

#[test]
fn help() {
    for arg in ["help add", "add -h", "add --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains("Add a dependency for a WebAssembly component"))
            .success();
    }
}

#[test]
fn requires_path_and_name() {
    cargo_component("add")
        .assert()
        .stderr(contains("--path <PATH>").and(contains("<name>")))
        .failure();
}

#[test]
fn validate_the_interface_file_exists() -> Result<()> {
    let project = Project::new("foo")?;
    project
        .cargo_component("add --path foo.wit foo")
        .assert()
        .stderr(contains("interface file `foo.wit` does not exist"))
        .failure();

    Ok(())
}

#[test]
fn checks_for_duplicate_exports() -> Result<()> {
    let project = Project::new("foo")?;

    project
        .cargo_component("add --path interface.wit --export export")
        .assert()
        .stderr(contains(
            "dependency `interface` already exists as the default interface",
        ))
        .failure();

    project
        .cargo_component("add --path interface.wit --version 0.1.0 --export export")
        .assert()
        .success();

    project
        .cargo_component("add --path interface.wit --version 0.1.0 --export export")
        .assert()
        .stderr(contains("dependency `export` already exists as an export"))
        .failure();

    Ok(())
}

#[test]
fn checks_for_duplicate_imports() -> Result<()> {
    let project = Project::new("foo")?;

    project
        .cargo_component("add --path interface.wit import")
        .assert()
        .stderr(contains("version not specified for import"))
        .failure();

    project
        .cargo_component("add --path interface.wit --version 0.1.0 import")
        .assert()
        .success();

    project
        .cargo_component("add --path interface.wit --version 0.1.0 import")
        .assert()
        .stderr(contains("dependency `import` already exists as an import"))
        .failure();

    Ok(())
}

#[test]
fn prints_modified_manifest_for_dry_run() -> Result<()> {
    let project = Project::new("foo")?;

    project
        .cargo_component("add --dry-run --path interface.wit --version 0.8.0 import")
        .assert()
        .stdout(contains(
            r#"import = { path = "interface.wit", version = "0.8.0" }"#,
        ))
        .success();

    // Assert the dependency was not added to the manifest
    assert!(!fs::read_to_string(project.root().join("Cargo.toml"))?.contains("import"));

    Ok(())
}
