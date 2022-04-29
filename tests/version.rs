use crate::support::*;
use assert_cmd::prelude::*;
use predicates::str::contains;

mod support;

#[test]
fn help() {
    for arg in ["-V", "--version"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(env!("CARGO_PKG_VERSION")))
            .success();
    }
}
