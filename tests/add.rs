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
fn validate_name_does_not_conflict_with_package() -> Result<()> {
    let project = Project::new("foo")?;
    project
        .cargo_component("add --path foo.wit foo")
        .assert()
        .stderr(contains(
            "cannot add dependency `foo` as it conflicts with the package name",
        ))
        .failure();

    Ok(())
}

#[test]
fn validate_the_interface_file_exists() -> Result<()> {
    let project = Project::new("foo")?;
    project
        .cargo_component("add --path bar.wit bar")
        .assert()
        .stderr(contains(
            "interface file `bar.wit` does not exist or is not a file",
        ))
        .failure();

    Ok(())
}

#[test]
fn checks_for_duplicate_exports() -> Result<()> {
    let project = Project::new("foo")?;

    project
        .cargo_component("add --path interface.wit --direct-export export")
        .assert()
        .stderr(contains(
            "a directly exported interface has already been specified in the manifest",
        ))
        .failure();

    project
        .cargo_component("add --path interface.wit --export export")
        .assert()
        .success();

    project
        .cargo_component("add --path interface.wit import")
        .assert()
        .success();

    project
        .cargo_component("add --path interface.wit --export export")
        .assert()
        .stderr(contains("an export with name `export` already exists"))
        .failure();

    project
        .cargo_component("add --path interface.wit --export import")
        .assert()
        .stderr(contains("an import with name `import` already exists"))
        .failure();

    Ok(())
}

#[test]
fn checks_for_duplicate_imports() -> Result<()> {
    let project = Project::new("foo")?;

    project
        .cargo_component("add --path interface.wit import")
        .assert()
        .success();

    project
        .cargo_component("add --path interface.wit --export export")
        .assert()
        .success();

    project
        .cargo_component("add --path interface.wit import")
        .assert()
        .stderr(contains("an import with name `import` already exists"))
        .failure();

    project
        .cargo_component("add --path interface.wit export")
        .assert()
        .stderr(contains("an export with name `export` already exists"))
        .failure();

    Ok(())
}

#[test]
fn prints_modified_manifest_for_dry_run() -> Result<()> {
    let project = Project::new("foo")?;

    project
        .cargo_component("add --dry-run --path interface.wit import")
        .assert()
        .stdout(contains(r#"import = "interface.wit""#))
        .success();

    // Assert the dependency was not added to the manifest
    assert!(!fs::read_to_string(project.root().join("Cargo.toml"))?.contains("import"));

    Ok(())
}
