use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use cargo_component::BINDINGS_CRATE_NAME;
use predicates::{prelude::PredicateBooleanExt, str::contains};
use toml_edit::value;

mod support;

#[test]
fn help() {
    for arg in ["help upgrade", "upgrade -h", "upgrade --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Install the latest version of cargo-component and upgrade to the corresponding version of cargo-component-bindings",
            ))
            .success();
    }
}

#[test]
fn upgrade_single_crate_already_current_is_no_op() -> Result<()> {
    let root = create_root()?;
    let project = Project::with_root(&root, "component", "")?;

    project
        .cargo_component("upgrade")
        .assert()
        .success()
        .stderr(contains(
            "Skipping package `component` as it already uses the current bindings crate version",
        ));

    Ok(())
}

#[test]
fn upgrade_single_crate_upgrades_bindings_dep() -> Result<()> {
    let root = create_root()?;
    let project = Project::with_root(&root, "component", "")?;
    project.update_manifest(|mut doc| {
        // Set arbitrary old version of bindings crate.
        doc["dependencies"][BINDINGS_CRATE_NAME] = value("0.1");
        Ok(doc)
    })?;

    // Check that the change actually stuck, and the old version
    // we set isn't the same as the current version.
    // (For symmetry with the assertion below that we actually
    // end up upgrading the bindings dep.)
    let manifest = project.read_manifest()?;
    assert_eq!(
        manifest["dependencies"][BINDINGS_CRATE_NAME].as_str(),
        Some("0.1")
    );
    assert_ne!(
        manifest["dependencies"][BINDINGS_CRATE_NAME].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );

    project
        .cargo_component("upgrade")
        .assert()
        .success()
        .stderr(contains("Updated "))
        .stderr(contains(format!(
            "from ^0.1 to {}",
            env!("CARGO_PKG_VERSION")
        )));

    // It should have actually written the upgrade.
    let manifest = project.read_manifest()?;
    assert_ne!(
        manifest["dependencies"][BINDINGS_CRATE_NAME].as_str(),
        Some("0.1")
    );
    assert_eq!(
        manifest["dependencies"][BINDINGS_CRATE_NAME].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );

    // A repeated upgrade should recognize that there is no change required.
    project
        .cargo_component("upgrade")
        .assert()
        .success()
        .stderr(contains(
            "Skipping package `component` as it already uses the current bindings crate version",
        ));

    Ok(())
}

#[test]
fn upgrade_dry_run_does_not_alter_manifest() -> Result<()> {
    let root = create_root()?;
    let project = Project::with_root(&root, "component", "")?;
    project.update_manifest(|mut doc| {
        // Set arbitrary old version of bindings crate.
        doc["dependencies"][BINDINGS_CRATE_NAME] = value("0.1");
        Ok(doc)
    })?;

    // Check that the change actually stuck, and the old version
    // we set isn't the same as the current version.
    // (For symmetry with the assertion below that we actually
    // end up upgrading the bindings dep.)
    let manifest = project.read_manifest()?;
    assert_eq!(
        manifest["dependencies"][BINDINGS_CRATE_NAME].as_str(),
        Some("0.1")
    );
    assert_ne!(
        manifest["dependencies"][BINDINGS_CRATE_NAME].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );

    project
        .cargo_component("upgrade --dry-run")
        .assert()
        .success()
        .stderr(contains("Would update "))
        .stderr(contains("Updated ").not())
        .stderr(contains(format!(
            "from ^0.1 to {}",
            env!("CARGO_PKG_VERSION")
        )));

    // It should NOT have written the upgrade.
    let manifest = project.read_manifest()?;
    assert_eq!(
        manifest["dependencies"][BINDINGS_CRATE_NAME].as_str(),
        Some("0.1")
    );
    assert_ne!(
        manifest["dependencies"][BINDINGS_CRATE_NAME].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );

    Ok(())
}
