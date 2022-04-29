use crate::support::*;
use assert_cmd::prelude::*;
use predicates::str::contains;

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
