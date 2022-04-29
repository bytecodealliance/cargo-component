use crate::support::*;
use assert_cmd::prelude::*;
use predicates::str::contains;

mod support;

#[test]
fn help() {
    for arg in ["help metadata", "metadata -h", "metadata --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains("Output the resolved dependencies of a package"))
            .success();
    }
}
