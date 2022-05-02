use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{boolean::PredicateBooleanExt, str::contains};
use std::{fmt::Write, fs};

mod support;

#[test]
fn help() {
    for arg in ["help check", "check -h", "check --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Check a local package and all of its dependencies for errors",
            ))
            .success();
    }
}

#[test]
fn it_checks_a_new_project() -> Result<()> {
    let project = Project::new("foo")?;
    project
        .cargo_component("check")
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
        .cargo_component("check")
        .assert()
        .stderr(
            contains("Checking interface v0.1.0")
                .and(contains("expected struct `String`, found `&str`")),
        )
        .failure();

    Ok(())
}
