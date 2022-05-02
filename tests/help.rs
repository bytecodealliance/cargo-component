use crate::support::*;
use assert_cmd::prelude::*;
use predicates::str::contains;

mod support;

#[test]
fn help() {
    for arg in ["help", "-h", "--help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains("Cargo integration for WebAssembly components"))
            .success();
    }
}

#[test]
fn shows_add_help() {
    cargo_component("help add")
        .assert()
        .stdout(contains("Add a dependency for a WebAssembly component"))
        .success();
}

#[test]
fn shows_build_help() {
    cargo_component("help build")
        .assert()
        .stdout(contains(
            "Compile a WebAssembly component and all of its dependencies",
        ))
        .success();
}

#[test]
fn shows_check_help() {
    cargo_component("help check")
        .assert()
        .stdout(contains(
            "Check a local package and all of its dependencies for errors",
        ))
        .success();
}

#[test]
fn shows_clippy_help() {
    cargo_component("help clippy")
        .assert()
        .stdout(contains(
            "Checks a package to catch common mistakes and improve your Rust code",
        ))
        .success();
}

#[test]
fn shows_metadata_help() {
    cargo_component("help metadata")
        .assert()
        .stdout(contains("Output the resolved dependencies of a package"))
        .success();
}

#[test]
fn shows_new_help() {
    cargo_component("help new")
        .assert()
        .stdout(contains(
            "Create a new WebAssembly component package at <path>",
        ))
        .success();
}
