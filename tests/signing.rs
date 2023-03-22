use crate::support::*;
use assert_cmd::prelude::*;
use predicates::str::contains;

mod support;

#[test]
fn help() {
    for arg in ["help signing", "signing -h", "signing --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Manage signing keys for publishing components to a registry",
            ))
            .success();
    }

    for arg in [
        "help signing new-key",
        "signing new-key -h",
        "signing new-key --help",
    ] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Creates a new signing key for a registry in the local keyring",
            ))
            .success();
    }

    for arg in [
        "help signing set-key",
        "signing set-key -h",
        "signing set-key --help",
    ] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Sets the signing key for a registry in the local keyring",
            ))
            .success();
    }

    for arg in [
        "help signing delete-key",
        "signing delete-key -h",
        "signing delete-key --help",
    ] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Deletes the signing key for a registry from the local keyring",
            ))
            .success();
    }
}

// NOTE: properly testing these commands requires access to the system keyring,
// and that may show a modal dialog that interferes with the test.
// Therefore, these commands are not fully tested here.
