use crate::support::*;
use assert_cmd::prelude::*;
use predicates::str::contains;

mod support;

#[test]
fn help() {
    for arg in ["help key", "key -h", "key --help"] {
        wit(arg)
            .assert()
            .stdout(contains(
                "Manage signing keys for publishing packages to a registry",
            ))
            .success();
    }

    for arg in ["help key new", "key new -h", "key new --help"] {
        wit(arg)
            .assert()
            .stdout(contains(
                "Creates a new signing key for a registry in the local keyring",
            ))
            .success();
    }

    for arg in ["help key set", "key set -h", "key set --help"] {
        wit(arg)
            .assert()
            .stdout(contains(
                "Sets the signing key for a registry in the local keyring",
            ))
            .success();
    }

    for arg in ["help key delete", "key delete -h", "key delete --help"] {
        wit(arg)
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
