use crate::support::*;
use assert_cmd::prelude::*;
use predicates::str::contains;

mod support;

#[test]
fn help() {
    for arg in ["help build", "build -h", "build --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Compile a WebAssembly component and all of its dependencies",
            ))
            .success();
    }
}
