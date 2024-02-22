use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::{str::contains, Predicate};
use std::{fs, rc::Rc};
use tempfile::TempDir;

mod support;

#[test]
fn help() {
    for arg in ["help new", "new -h", "new --help"] {
        cargo_component(arg)
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

    cargo_component("new --bin foo")
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

    cargo_component("new --lib foo")
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

    cargo_component("new --lib foo --editor none")
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

    cargo_component("new --lib foo --edition 2018")
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

    cargo_component("new --lib foo --name bar")
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

    cargo_component("new --lib foo --name fn")
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
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    publish_wit(
        &config,
        "test:bar",
        "1.2.3",
        r#"package test:bar@1.2.3;
world foo {
    resource file {
        open: static func(path: string) -> file;
        path: func() -> string;
    }
    import foo: func() -> file;
    export bar: func(file: borrow<file>) -> file;
}"#,
        true,
    )
    .await?;

    let project = Project::with_dir(dir.clone(), "component", "--target test:bar@1.0.0")?;

    project
        .cargo_component("build")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();
    validate_component(&project.debug_wasm("component"))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_errors_if_target_does_not_exist() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    match Project::with_dir(dir.clone(), "component", "--target foo:bar@1.0.0") {
        Ok(_) => panic!("expected error"),
        Err(e) => assert!(contains("package `foo:bar` does not exist").eval(&e.to_string())),
    }

    Ok(())
}

#[test]
fn it_supports_the_command_option() -> Result<()> {
    let dir = TempDir::new()?;

    cargo_component("new --command foo")
        .current_dir(dir.path())
        .assert()
        .try_success()?;

    Ok(())
}

#[test]
fn it_supports_the_reactor_option() -> Result<()> {
    let dir = TempDir::new()?;

    cargo_component("new --reactor foo")
        .current_dir(dir.path())
        .assert()
        .try_success()?;

    Ok(())
}

#[test]
fn it_supports_the_proxy_option() -> Result<()> {
    let dir: TempDir = TempDir::new()?;

    cargo_component("new --lib --proxy foo")
        .current_dir(dir.path())
        .assert()
        .try_success()?;

    assert!(fs::read_to_string(dir.path().join("foo/Cargo.toml"))?.contains("proxy = true"));

    Ok(())
}
