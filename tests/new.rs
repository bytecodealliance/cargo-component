use std::fs;

use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{str::contains, Predicate};
use tempfile::TempDir;
use wasm_pkg_client::warg::WargRegistryConfig;

use crate::support::*;

mod support;

#[test]
fn help() {
    for arg in ["help new", "new -h", "new --help"] {
        cargo_component(arg.split_whitespace())
            .assert()
            .stdout(contains(
                "Create a new WebAssembly component package at <path>",
            ))
            .success();
    }
}

#[test]
fn it_creates_the_expected_files_for_bin() -> Result<()> {
    let dir = TempDir::new()?;

    cargo_component(["new", "--bin", "foo"])
        .current_dir(dir.path())
        .assert()
        .stderr(contains("Updated manifest of package `foo"))
        .success();

    let proj_dir = dir.path().join("foo");

    assert!(proj_dir.join("Cargo.toml").is_file());
    assert!(!proj_dir.join("wit/world.wit").is_file());
    assert!(!proj_dir.join("src").join("lib.rs").is_file());
    assert!(proj_dir.join("src").join("main.rs").is_file());
    assert!(proj_dir.join(".vscode").join("settings.json").is_file());

    Ok(())
}

#[test]
fn it_creates_the_expected_files() -> Result<()> {
    let dir = TempDir::new()?;

    cargo_component(["new", "--lib", "foo"])
        .current_dir(dir.path())
        .assert()
        .stderr(contains("Updated manifest of package `foo`"))
        .success();

    let proj_dir = dir.path().join("foo");

    assert!(proj_dir.join("Cargo.toml").is_file());
    assert!(proj_dir.join("wit/world.wit").is_file());
    assert!(proj_dir.join("src").join("lib.rs").is_file());
    assert!(!proj_dir.join("src").join("main.rs").is_file());
    assert!(proj_dir.join(".vscode").join("settings.json").is_file());

    Ok(())
}

#[test]
fn it_supports_editor_option() -> Result<()> {
    let dir = TempDir::new()?;

    cargo_component(["new", "--lib", "foo", "--editor", "none"])
        .current_dir(dir.path())
        .assert()
        .stderr(contains("Updated manifest of package `foo"))
        .success();

    let proj_dir = dir.path().join("foo");

    assert!(proj_dir.join("Cargo.toml").is_file());
    assert!(proj_dir.join("wit/world.wit").is_file());
    assert!(proj_dir.join("src").join("lib.rs").is_file());
    assert!(!proj_dir.join(".vscode").is_dir());

    Ok(())
}

#[test]
fn it_supports_edition_option() -> Result<()> {
    let dir = TempDir::new()?;

    cargo_component(["new", "--lib", "foo", "--edition", "2018"])
        .current_dir(dir.path())
        .assert()
        .stderr(contains("Updated manifest of package `foo"))
        .success();

    let proj_dir = dir.path().join("foo");

    assert!(fs::read_to_string(proj_dir.join("Cargo.toml"))?.contains("edition = \"2018\""));

    Ok(())
}

#[test]
fn it_supports_name_option() -> Result<()> {
    let dir = TempDir::new()?;

    cargo_component(["new", "--lib", "foo", "--name", "bar"])
        .current_dir(dir.path())
        .assert()
        .stderr(contains("Updated manifest of package `bar`"))
        .success();

    let proj_dir = dir.path().join("foo");

    assert!(fs::read_to_string(proj_dir.join("Cargo.toml"))?.contains("name = \"bar\""));

    Ok(())
}

#[test]
fn it_rejects_rust_keywords() -> Result<()> {
    let dir = TempDir::new()?;

    cargo_component(["new", "--lib", "foo", "--name", "fn"])
        .current_dir(dir.path())
        .assert()
        .stderr(contains(
            "the name `fn` cannot be used as a package name, it is a Rust keyword",
        ))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_targets_a_world() -> Result<()> {
    let (server, config, registry) = spawn_server(Vec::<String>::new()).await?;

    let warg_config =
        WargRegistryConfig::try_from(config.registry_config(&registry).unwrap()).unwrap();

    publish_wit(
        &warg_config.client_config,
        "test:bar",
        "1.2.3",
        r#"package test:bar@1.2.3;

interface bar {
    resource a {
        constructor(a: borrow<a>);
        a: static func(a: borrow<a>) -> a;
        b: func(a: a);
    }

    resource b {
        constructor(a: borrow<a>);
        a: static func(a: borrow<a>) -> a;
        b: func(a: a);
    }

    w: func(a: a) -> a;
    x: func(b: b) -> b;
    y: func(a: borrow<a>);
    z: func(b: borrow<b>);
}

interface not-exported {
    resource some {
        hello: func() -> string;
    }
}

interface is-exported {
    resource else {
        hello: func() -> string;
    }
}

interface is-exported-and-aliased {
    resource something {
        hello: func() -> string;
    }
}

interface baz {
    use bar.{a as a2};
    use not-exported.{some};
    use is-exported.{else};
    use is-exported-and-aliased.{something as someelse};

    resource a {
        constructor(a: borrow<a>);
        a: static func(a: borrow<a>) -> a;
        b: func(a: a);
        c: static func(some: borrow<some>) -> a;
        d: static func(else: borrow<else>) -> a;
        e: static func(else: borrow<someelse>) -> a;
    }

    resource b {
        constructor(a: borrow<a>);
        a: static func(a: borrow<a>) -> a;
        b: func(a: a);
    }

    u: func(some: borrow<some>) -> some;
    v: func(a2: borrow<a2>) -> a2;
    w: func(a: a) -> a;
    x: func(b: b) -> b;
    y: func(a: borrow<a>);
    z: func(b: borrow<b>);
}

interface another {
    resource yet-another {
        hello: func() -> string;
    }
    resource empty {}
}

world not-used {
    export not-exported;
}

world foo {
    use another.{yet-another, empty};

    resource file {
        open: static func(path: string) -> file;
        path: func() -> string;
    }
    import foo: func() -> file;
    export bar: func(file: borrow<file>) -> file;
    export bar;
    export baz;
    export is-exported;
    export is-exported-and-aliased;

    export another;
    export another-func: func(yet: borrow<yet-another>) -> result;
    export another-func-empty: func(empty: borrow<empty>) -> result;
}"#,
        true,
    )
    .await?;

    let project = server.project("component", true, ["--target", "test:bar/foo@1.0.0"])?;
    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_errors_if_target_does_not_exist() -> Result<()> {
    let (server, _, _) = spawn_server(["foo"]).await?;

    match server.project("component", true, ["--target", "foo:bar@1.0.0"]) {
        Ok(_) => panic!("expected error"),
        Err(e) => assert!(
            contains("package `foo:bar` was not found").eval(&e.to_string()),
            "Should contain error message {e:?}"
        ),
    }

    Ok(())
}

#[test]
fn it_supports_the_command_option() -> Result<()> {
    let dir = TempDir::new()?;

    cargo_component(["new", "--command", "foo"])
        .current_dir(dir.path())
        .assert()
        .try_success()?;

    Ok(())
}

#[test]
fn it_supports_the_reactor_option() -> Result<()> {
    let dir = TempDir::new()?;

    cargo_component(["new", "--reactor", "foo"])
        .current_dir(dir.path())
        .assert()
        .try_success()?;

    Ok(())
}

#[test]
fn it_supports_the_proxy_option() -> Result<()> {
    let dir: TempDir = TempDir::new()?;

    cargo_component(["new", "--lib", "--proxy", "foo"])
        .current_dir(dir.path())
        .assert()
        .try_success()?;

    assert!(fs::read_to_string(dir.path().join("foo/Cargo.toml"))?.contains("proxy = true"));

    Ok(())
}
