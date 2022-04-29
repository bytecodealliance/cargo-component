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
