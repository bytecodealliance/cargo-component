use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{boolean::PredicateBooleanExt, str::contains};
use std::{fmt::Write, fs};

mod support;

#[test]
fn help() {
    for arg in ["help clippy", "clippy -h", "clippy --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Checks a package to catch common mistakes and improve your Rust code",
            ))
            .success();
    }
}

#[test]
fn it_checks_a_new_project() -> Result<()> {
    let project = Project::new("foo")?;
    project
        .cargo_component("clippy")
        .assert()
        .stderr(contains("Checking interface v0.1.0"))
        .success();

    Ok(())
}

#[test]
fn it_finds_errors() -> Result<()> {
    let project = Project::new("foo")?;

    let mut src = fs::read_to_string(project.root().join("src/lib.rs"))?;
    write!(&mut src, "\n\nfn foo() -> String {{\n  \"foo\"\n}}\n")?;

    fs::write(project.root().join("src/lib.rs"), src)?;

    project
        .cargo_component("clippy")
        .assert()
        .stderr(contains("Checking interface v0.1.0").and(contains(
            "expected struct `std::string::String`, found `&str`",
        )))
        .failure();

    Ok(())
}

#[test]
fn it_finds_clippy_warnings() -> Result<()> {
    let project = Project::new("foo")?;

    let mut src = fs::read_to_string(project.root().join("src/lib.rs"))?;
    write!(
        &mut src,
        "\n\nfn foo() -> String {{\n  return \"foo\".to_string();\n}}\n"
    )?;

    fs::write(project.root().join("src/lib.rs"), src)?;

    project
        .cargo_component("clippy")
        .assert()
        .stderr(contains("Checking interface v0.1.0").and(contains("clippy::needless_return")))
        .success();

    Ok(())
}
