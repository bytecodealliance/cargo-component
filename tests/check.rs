use crate::support::*;
use assert_cmd::prelude::*;
use predicates::str::contains;

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
