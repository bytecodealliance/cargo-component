use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{prelude::PredicateBooleanExt, str::contains};
use std::fs;

mod support;

#[test]
fn help() {
    for arg in ["help update", "update -h", "update --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Update dependencies as recorded in the local lock files",
            ))
            .success();
    }
}

#[test]
fn test_update_without_changes_is_a_noop() -> Result<()> {
    let root = create_root()?;
    let path = root.join("registry");
    fs::write(
        root.join("foo.wit"),
        r#"default world foo {
    import foo: func() -> string
    export bar: func() -> string
}"#,
    )?;

    cargo_component(&format!(
        "registry publish --registry {path} --id foo/bar --version 1.0.0 foo.wit",
        path = path.display()
    ))
    .current_dir(&root)
    .assert()
    .success();

    let project = Project::with_root(
        root,
        "component",
        &format!(
            "--registry {path} --target foo/bar@1.0.0",
            path = path.display()
        ),
    )?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    project
        .cargo_component("update")
        .assert()
        .success()
        .stderr(contains("foo/bar").not());

    Ok(())
}

#[test]
fn test_update_without_compatible_changes_is_a_noop() -> Result<()> {
    let root = create_root()?;
    let path = root.join("registry");
    fs::write(
        root.join("foo.wit"),
        r#"default world foo {
    import foo: func() -> string
    export bar: func() -> string
}"#,
    )?;

    cargo_component(&format!(
        "registry publish --registry {path} --id foo/bar --version 1.0.0 foo.wit",
        path = path.display()
    ))
    .current_dir(&root)
    .assert()
    .success();

    let project = Project::with_root(
        root,
        "component",
        &format!(
            "--registry {path} --target foo/bar@1.0.0",
            path = path.display()
        ),
    )?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    fs::write(
        project.root().join("foo.wit"),
        r#"default world foo {
    export bar: func() -> string
}"#,
    )?;

    project
        .cargo_component(&format!(
            "registry publish --registry {path} --id foo/bar --version 2.0.0 foo.wit",
            path = path.display()
        ))
        .assert()
        .success();

    project
        .cargo_component("update")
        .assert()
        .success()
        .stderr(contains("foo/bar").not());

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    Ok(())
}

#[test]
fn test_update_with_compatible_changes() -> Result<()> {
    let root = create_root()?;
    let path = root.join("registry");
    fs::write(
        root.join("foo.wit"),
        r#"default world foo {
    import foo: func() -> string
    export bar: func() -> string
}"#,
    )?;

    cargo_component(&format!(
        "registry publish --registry {path} --id foo/bar --version 1.0.0 foo.wit",
        path = path.display()
    ))
    .current_dir(&root)
    .assert()
    .success();

    let project = Project::with_root(
        root,
        "component",
        &format!(
            "--registry {path} --target foo/bar@1.0.0",
            path = path.display()
        ),
    )?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    fs::write(
        project.root().join("../foo.wit"),
        r#"default world foo {
    import foo: func() -> string
    import baz: func() -> string
    export bar: func() -> string
}"#,
    )?;

    project
        .cargo_component(&format!(
            "registry publish --registry {path} --id foo/bar --version 1.1.0 ../foo.wit",
            path = path.display()
        ))
        .assert()
        .success();

    project
        .cargo_component("update")
        .assert()
        .success()
        .stderr(contains("`foo/bar` v1.0.0 -> v1.1.0"));

    let source = r#"use bindings::{baz, Foo};
struct Component;
impl Foo for Component {
    fn bar() -> String {
        baz()
    }
}
bindings::export!(Component);
"#;

    fs::write(project.root().join("src/lib.rs"), source)?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    Ok(())
}
