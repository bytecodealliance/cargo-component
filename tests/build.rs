use std::{fs, process::Command, rc::Rc};

use anyhow::{Context, Result};
use assert_cmd::prelude::*;
use predicates::{prelude::PredicateBooleanExt, str::contains};
use tempfile::TempDir;
use toml_edit::{value, Array, Item, Table};

use crate::support::*;

mod support;

#[test]
fn it_builds_debug() -> Result<()> {
    let project = Project::new("foo", true)?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(&project.debug_wasm("foo"))?;

    // A lock file should only be generated for projects with
    // registry dependencies
    assert!(!project.root().join("Cargo-component.lock").exists());

    Ok(())
}

#[test]
fn it_builds_a_bin_project_with_snake_case() -> Result<()> {
    let project = Project::new("hello_world", false)?;

    project
        .cargo_component(["build", "--release"])
        .assert()
        .stderr(contains("Finished `release` profile [optimized] target(s)"))
        .success();

    validate_component(&project.release_wasm("hello_world"))?;

    Ok(())
}

#[test]
fn it_builds_a_bin_project() -> Result<()> {
    let project = Project::new("foo", false)?;

    project
        .cargo_component(["build", "--release"])
        .assert()
        .stderr(contains("Finished `release` profile [optimized] target(s)"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    Ok(())
}

#[test]
fn it_builds_a_workspace() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let project = Project::new_uninitialized(dir.clone(), dir.path().to_owned());

    project.file(
        "baz/Cargo.toml",
        r#"[package]
name = "baz"
version = "0.1.0"
edition = "2024"

[dependencies]
"#,
    )?;

    project.file("baz/src/lib.rs", "")?;

    project
        .cargo_component(["new", "--lib", "foo"])
        .assert()
        .stderr(contains("Updated manifest of package `foo`"))
        .success();

    project
        .cargo_component(["new", "--lib", "bar"])
        .assert()
        .stderr(contains("Updated manifest of package `bar`"))
        .success();

    // Add the workspace after all of the projects have been created
    project.file(
        "Cargo.toml",
        r#"[workspace]
    members = ["foo", "bar", "baz"]
    "#,
    )?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(&project.debug_wasm("foo"))?;
    validate_component(&project.debug_wasm("bar"))?;

    Ok(())
}

#[test]
fn it_supports_wit_keywords() -> Result<()> {
    let project = Project::new("interface", true)?;

    project
        .cargo_component(["build", "--release"])
        .assert()
        .stderr(contains("Finished `release` profile [optimized] target(s)"))
        .success();

    validate_component(&project.release_wasm("interface"))?;

    Ok(())
}

#[test]
fn it_builds_wasm32_unknown_unknown_from_cli() -> Result<()> {
    let project = Project::new("foo", true)?;

    project
        .cargo_component(["build", "--target", "wasm32-unknown-unknown"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(
        &project
            .build_dir()
            .join("wasm32-unknown-unknown")
            .join("debug")
            .join("foo.wasm"),
    )?;

    Ok(())
}

#[test]
fn it_builds_wasm32_unknown_unknown_from_config() -> Result<()> {
    let project = Project::new("foo", true)?;

    project.file(
        ".cargo/config.toml",
        r#"[build]
target = "wasm32-unknown-unknown"
    "#,
    )?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(
        &project
            .build_dir()
            .join("wasm32-unknown-unknown")
            .join("debug")
            .join("foo.wasm"),
    )?;

    Ok(())
}

#[test]
fn it_builds_wasm32_unknown_unknown_from_env() -> Result<()> {
    let project = Project::new("foo", true)?;

    project
        .cargo_component(["build"])
        .env("CARGO_BUILD_TARGET", "wasm32-unknown-unknown")
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(
        &project
            .build_dir()
            .join("wasm32-unknown-unknown")
            .join("debug")
            .join("foo.wasm"),
    )?;

    Ok(())
}

#[test]
fn it_regenerates_target_if_wit_changed() -> Result<()> {
    let project = Project::new("foo", true)?;
    project.update_manifest(|mut doc| {
        doc["package"]["metadata"]["component"]["target"]["world"] = value("example");
        Ok(doc)
    })?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(&project.debug_wasm("foo"))?;

    fs::write(project.root().join("wit/other.wit"), "world foo {}")?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains("Generating bindings"))
        .success();

    Ok(())
}

#[test]
fn it_builds_with_local_wit_deps() -> Result<()> {
    let project = Project::new("foo", true)?;
    project.update_manifest(|mut doc| {
        let mut dependencies = Table::new();
        dependencies["foo:bar"]["path"] = value("wit/deps/foo-bar");
        dependencies["bar:baz"]["path"] = value("wit/deps/bar-baz/qux.wit");
        dependencies["baz:qux"]["path"] = value("wit/deps/foo-bar/deps/baz-qux/qux.wit");

        let target =
            doc["package"]["metadata"]["component"]["target"].or_insert(Item::Table(Table::new()));
        target["dependencies"] = Item::Table(dependencies);
        Ok(doc)
    })?;

    // Create the foo-bar wit package
    fs::create_dir_all(project.root().join("wit/deps/foo-bar/deps/baz-qux"))?;
    fs::write(
        project.root().join("wit/deps/foo-bar/deps/baz-qux/qux.wit"),
        "package baz:qux;

interface qux {
    type ty = u32;
}",
    )?;

    fs::write(
        project.root().join("wit/deps/foo-bar/bar.wit"),
        "package foo:bar;

interface baz {
    use baz:qux/qux.{ty};
    baz: func() -> ty;
}",
    )?;

    fs::create_dir_all(project.root().join("wit/deps/bar-baz"))?;

    fs::write(
        project.root().join("wit/deps/bar-baz/qux.wit"),
        "package bar:baz;
interface qux {
    use baz:qux/qux.{ty};
    qux: func();
}",
    )?;

    fs::write(
        project.root().join("wit/world.wit"),
        "package component:foo;

world example {
    export foo:bar/baz;
    export bar:baz/qux;
}",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        "#[allow(warnings)]
mod bindings;
use bindings::exports::{foo::bar::baz::{Guest as Baz, Ty}, bar::baz::qux::Guest as Qux};

struct Component;

impl Baz for Component {
    fn baz() -> Ty {
        todo!()
    }
}

impl Qux for Component {
    fn qux() {
        todo!()
    }
}

bindings::export!(Component with_types_in bindings);
",
    )?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(&project.debug_wasm("foo"))?;

    Ok(())
}

#[test]
fn empty_world_with_dep_valid() -> Result<()> {
    let project = Project::new("dep", true)?;

    fs::write(
        project.root().join("wit/world.wit"),
        "
            package foo:bar;

            world the-world {
                flags foo {
                    bar
                }

                export hello: func() -> list<foo>;
            }
        ",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        "
            #[allow(warnings)]
            mod bindings;
            use bindings::{Guest, Foo};
            struct Component;

            impl Guest for Component {
                fn hello() -> Vec<Foo> {
                    vec![Foo::BAR]
                }
            }

            bindings::export!(Component with_types_in bindings);
        ",
    )?;

    project.cargo_component(["build"]).assert().success();

    let dep = project.debug_wasm("dep");
    validate_component(&dep)?;

    let project = Project::with_dir(project.dir().clone(), "main", true, Vec::<String>::new())?;
    project.update_manifest(|mut doc| {
        let table = doc["package"]["metadata"]["component"]
            .as_table_mut()
            .unwrap();
        table.remove("package");
        table.remove("target");
        let mut dependencies = Table::new();
        dependencies["foo:bar"]["path"] = value(dep.display().to_string());
        doc["package"]["metadata"]["component"]["dependencies"] = Item::Table(dependencies);
        Ok(doc)
    })?;

    fs::remove_dir_all(project.root().join("wit"))?;

    fs::write(
        project.root().join("src/lib.rs"),
        "
            #[allow(warnings)]
            mod bindings;

            #[unsafe(no_mangle)]
            pub extern \"C\" fn foo() {
                bindings::foo_bar::hello();
            }
        ",
    )?;

    project.cargo_component(["build"]).assert().success();
    validate_component(&project.debug_wasm("main"))?;

    Ok(())
}

#[test]
fn it_builds_with_resources() -> Result<()> {
    let project = Project::new("foo", true)?;

    fs::write(
        project.root().join("wit/world.wit"),
        "
            package foo:bar;

            world bar {
                export baz: interface {
                    resource keyed-integer {
                        constructor(x: u32);
                        get: func() -> u32;
                        set: func(x: u32);
                        key: static func() -> string;
                    }
                }
            }
        ",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        r#"
            #[allow(warnings)]
            mod bindings;

            use std::cell::Cell;

            struct Component;

            impl bindings::exports::baz::Guest for Component {
                type KeyedInteger = KeyedInteger;
            }

            bindings::export!(Component with_types_in bindings);

            pub struct KeyedInteger(Cell<u32>);

            impl bindings::exports::baz::GuestKeyedInteger for KeyedInteger {
                fn new(x: u32) -> Self {
                    Self(Cell::new(x))
                }

                fn get(&self) -> u32 {
                    self.0.get()
                }

                fn set(&self, x: u32) {
                    self.0.set(x);
                }

                fn key() -> String {
                    "my-key".to_string()
                }
            }
        "#,
    )?;

    project.cargo_component(["build"]).assert().success();

    let dep = project.debug_wasm("foo");
    validate_component(&dep)?;

    Ok(())
}

#[test]
fn it_builds_resources_with_specified_ownership_model() -> Result<()> {
    let project = Project::new("foo", true)?;
    project.update_manifest(|mut doc| {
        doc["package"]["metadata"]["component"]["bindings"]["ownership"] =
            value("borrowing-duplicate-if-necessary");
        Ok(doc)
    })?;

    fs::write(
        project.root().join("wit/world.wit"),
        "
            package foo:bar;

            world bar {
                export baz: interface {
                    resource keyed-integer {
                        constructor(x: u32);
                        get: func() -> u32;
                        set: func(x: u32);
                        key: static func() -> string;
                    }
                }
            }
        ",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        r#"
            #[allow(warnings)]
            mod bindings;

            use std::cell::Cell;

            struct Component;

            impl bindings::exports::baz::Guest for Component {
                type KeyedInteger = KeyedInteger;
            }

            bindings::export!(Component with_types_in bindings);

            pub struct KeyedInteger(Cell<u32>);

            impl bindings::exports::baz::GuestKeyedInteger for KeyedInteger {
                fn new(x: u32) -> Self {
                    Self(Cell::new(x))
                }

                fn get(&self) -> u32 {
                    self.0.get()
                }

                fn set(&self, x: u32) {
                    self.0.set(x);
                }

                fn key() -> String {
                    "my-key".to_string()
                }
            }
        "#,
    )?;

    project.cargo_component(["build"]).assert().success();

    let dep = project.debug_wasm("foo");
    validate_component(&dep)?;

    Ok(())
}

#[test]
fn it_builds_with_a_component_dependency() -> Result<()> {
    let comp1 = Project::new("comp1", true)?;

    fs::write(
        comp1.root().join("wit/world.wit"),
        "
package my:comp1;

interface types {
    record seed {
        value: u32,
    }
}

interface types2 {
    type seed = u32;
}

interface other {
    use types2.{seed};
    rand: func(seed: seed) -> u32;
}

world random-generator {
    use types.{seed};
    export rand: func(seed: seed) -> u32;
    export other;
}
",
    )?;

    fs::write(
        comp1.root().join("src/lib.rs"),
        r#"
#[allow(warnings)]
mod bindings;

use bindings::{Guest, Seed, exports::my::comp1::other};

struct Component;

impl Guest for Component {
    fn rand(seed: Seed) -> u32 {
        seed.value + 1
    }
}

impl other::Guest for Component {
    fn rand(seed: other::Seed) -> u32 {
        seed + 2
    }
}

bindings::export!(Component with_types_in bindings);
"#,
    )?;

    comp1
        .cargo_component(["build", "--release"])
        .assert()
        .stderr(contains("Finished `release` profile [optimized] target(s)"))
        .success();

    let dep = comp1.release_wasm("comp1");
    validate_component(&dep)?;

    let comp2 = Project::with_dir(comp1.dir.clone(), "comp2", true, Vec::<String>::new())?;
    comp2.update_manifest(|mut doc| {
        doc["package"]["metadata"]["component"]["dependencies"]["my:comp1"]["path"] =
            value(dep.display().to_string());
        Ok(doc)
    })?;

    fs::write(
        comp2.root().join("wit/world.wit"),
        "
package my:comp2;

world random-generator {
    export rand: func() -> u32;
}
",
    )?;

    fs::write(
        comp2.root().join("src/lib.rs"),
        r#"
#[allow(warnings)]
mod bindings;

use bindings::{Guest, my_comp1, my};

struct Component;

impl Guest for Component {
    fn rand() -> u32 {
        my_comp1::rand(my_comp1::Seed { value: 1 }) + my::comp1::my_comp1_other::rand(1)
    }
}

bindings::export!(Component with_types_in bindings);
"#,
    )?;

    comp2
        .cargo_component(["build", "--release"])
        .assert()
        .stderr(contains("Finished `release` profile [optimized] target(s)"))
        .success();

    let path: std::path::PathBuf = comp2.release_wasm("comp2");
    validate_component(&path)?;

    Ok(())
}

#[test]
fn it_builds_with_adapter() -> Result<()> {
    let project = Project::new("foo", true)?;
    project.update_manifest(|mut doc| {
        doc["package"]["metadata"]["component"]["adapter"] = value("not-a-valid-path");
        Ok(doc)
    })?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains("error: failed to read module adapter"))
        .failure();

    let project = Project::new("foo", true)?;
    let adapter_path = "adapter/wasi_snapshot_preview1.wasm";
    project.file(
        adapter_path,
        wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_REACTOR_ADAPTER,
    )?;
    project.update_manifest(|mut doc| {
        doc["package"]["metadata"]["component"]["adapter"] = value(adapter_path);
        Ok(doc)
    })?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(&project.debug_wasm("foo"))?;

    Ok(())
}

#[test]
fn it_errors_if_adapter_is_not_wasm() -> Result<()> {
    let project = Project::new("foo", true)?;
    project.update_manifest(|mut doc| {
        doc["package"]["metadata"]["component"]["adapter"] = value("foo.wasm");
        Ok(doc)
    })?;

    fs::write(project.root().join("foo.wasm"), "not wasm")?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains("error: failed to load adapter module"))
        .failure();

    Ok(())
}

#[test]
fn it_adds_additional_derives() -> Result<()> {
    let project = Project::new("foo", true)?;
    project.update_manifest(|mut doc| {
        doc["package"]["metadata"]["component"]["bindings"]["derives"] =
            value(Array::from_iter(["serde::Serialize", "serde::Deserialize"]));
        Ok(doc)
    })?;

    std::process::Command::new("cargo")
        .args(["add", "serde", "--features", "derive"])
        .current_dir(project.root())
        .assert()
        .success();
    std::process::Command::new("cargo")
        .args(["add", "serde_json"])
        .current_dir(project.root())
        .assert()
        .success();

    fs::write(
        project.root().join("wit/world.wit"),
        "
package my:derive;

interface foo {
    record bar {
        value: u32,
    }
}

world foo-world {
    use foo.{bar};

    export baz: func(thing: bar) -> list<u8>;
}
",
    )?;
    fs::write(
        project.root().join("src/lib.rs"),
        r#"
#[allow(warnings)]
mod bindings;
use bindings::Guest;
use bindings::my::derive::foo::Bar;

struct Component;

impl Guest for Component {
    fn baz(thing: Bar) -> Vec<u8> {
        let stuff = serde_json::to_vec(&thing).unwrap();
        // Check we got the derived deserialize
        let _thing: Bar = serde_json::from_slice(&stuff).unwrap();
        stuff
    }
}

bindings::export!(Component with_types_in bindings);
"#,
    )?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(&project.debug_wasm("foo"))?;

    Ok(())
}

#[test]
fn it_builds_with_versioned_wit() -> Result<()> {
    let project = Project::new("foo", true)?;

    fs::write(
        project.root().join("wit/world.wit"),
        "
            package foo:bar@1.2.3;

            interface foo {
                f: func();
            }

            world bar {
                export foo;
            }
        ",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        r#"
            #[allow(warnings)]
            mod bindings;

            struct Component;

            impl bindings::exports::foo::bar::foo::Guest for Component {
                fn f() {}
            }

            bindings::export!(Component with_types_in bindings);
        "#,
    )?;

    project.cargo_component(["build"]).assert().success();

    let dep = project.debug_wasm("foo");
    validate_component(&dep)?;

    Ok(())
}

#[test]
fn it_warns_on_proxy_setting_for_command() -> Result<()> {
    let project = Project::new("foo", false)?;
    project.update_manifest(|mut doc| {
        doc["package"]["metadata"]["component"]["proxy"] = value(true);
        Ok(doc)
    })?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "warning: ignoring `proxy` setting in `Cargo.toml` for command component",
        ))
        .success();

    validate_component(&project.debug_wasm("foo"))?;

    Ok(())
}

#[test]
fn it_warns_with_proxy_and_adapter_settings() -> Result<()> {
    let project = Project::new("foo", true)?;
    let adapter_path = "adapter/wasi_snapshot_preview1.wasm";
    project.file(
        adapter_path,
        wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_REACTOR_ADAPTER,
    )?;
    project.update_manifest(|mut doc| {
        doc["package"]["metadata"]["component"]["proxy"] = value(true);
        doc["package"]["metadata"]["component"]["adapter"] = value(adapter_path);
        Ok(doc)
    })?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains("warning: ignoring `proxy` setting due to `adapter` setting being present in `Cargo.toml`"))
        .success();

    validate_component(&project.debug_wasm("foo"))?;

    Ok(())
}

#[test]
fn it_builds_with_proxy_adapter() -> Result<()> {
    let project = Project::new_with_args("foo", true, ["--proxy"])?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(&project.debug_wasm("foo"))?;

    let text = wasmprinter::print_file(project.debug_wasm("foo"))?;
    assert!(
        !text.contains("wasi:cli/environment"),
        "proxy wasm should have no reference to `wasi:cli/environment`"
    );

    Ok(())
}

#[test]
fn it_does_not_generate_bindings_for_cargo_projects() -> Result<()> {
    let dir = TempDir::new()?;

    for (name, args) in [("foo", &["new", "--lib"] as &[_]), ("bar", &["new"])] {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(dir.path());
        cmd.args(args);
        cmd.arg(name);
        cmd.assert().stderr(contains("Creating")).success();

        let mut cmd = cargo_component(["build"]);
        cmd.current_dir(dir.path().join(name));
        cmd.assert()
            .stderr(contains("Generating bindings").not())
            .success();
    }

    Ok(())
}

#[test]
/// This is exactly the `it_builds_a_workspace` test with just the edition changed to 2021.
fn it_supports_edition_2021() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let project = Project::new_uninitialized(dir.clone(), dir.path().to_owned());

    project.file(
        "baz/Cargo.toml",
        r#"[package]
name = "baz"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
    )?;

    project.file("baz/src/lib.rs", "")?;

    project
        .cargo_component(["new", "--lib", "foo"])
        .assert()
        .stderr(contains("Updated manifest of package `foo`"))
        .success();

    project
        .cargo_component(["new", "--lib", "bar"])
        .assert()
        .stderr(contains("Updated manifest of package `bar`"))
        .success();

    // Add the workspace after all of the projects have been created
    project.file(
        "Cargo.toml",
        r#"[workspace]
    members = ["foo", "bar", "baz"]
    "#,
    )?;

    project
        .cargo_component(["build"])
        .assert()
        .stderr(contains(
            "Finished `dev` profile [unoptimized + debuginfo] target(s)",
        ))
        .success();

    validate_component(&project.debug_wasm("foo"))?;
    validate_component(&project.debug_wasm("bar"))?;

    Ok(())
}

#[test]
fn it_adds_a_producers_field() -> Result<()> {
    let project = Project::new("foo", true)?;

    project
        .cargo_component(["build", "--release"])
        .assert()
        .stderr(contains("Finished `release` profile [optimized] target(s)"))
        .success();

    let path = project.release_wasm("foo");

    validate_component(&path)?;

    let wasm = fs::read(&path)
        .with_context(|| format!("failed to read wasm file `{path}`", path = path.display()))?;
    let section = wasm_metadata::Producers::from_wasm(&wasm)?.expect("missing producers section");

    assert_eq!(
        section
            .get("processed-by")
            .expect("missing processed-by field")
            .get(env!("CARGO_PKG_NAME"))
            .expect("missing cargo-component field"),
        option_env!("CARGO_VERSION_INFO").unwrap_or(env!("CARGO_PKG_VERSION"))
    );

    Ok(())
}

#[test]
fn it_adds_metadata_from_cargo_toml() -> Result<()> {
    let name = "foo";
    let authors = "Jane Doe <jane@example.com>";
    let description = "A test package";
    let license = "Apache-2.0";
    let version = "1.0.0";
    let documentation = "https://example.com/docs";
    let homepage = "https://example.com/home";
    let repository = "https://example.com/repo";

    let project = Project::new(name, true)?;
    project.update_manifest(|mut doc| {
        let package = &mut doc["package"];
        package["name"] = value(name);
        package["version"] = value(version);
        package["authors"] = value(Array::from_iter([authors]));
        package["description"] = value(description);
        package["license"] = value(license);
        package["documentation"] = value(documentation);
        package["homepage"] = value(homepage);
        package["repository"] = value(repository);
        Ok(doc)
    })?;

    project
        .cargo_component(["build", "--release"])
        .assert()
        .stderr(contains("Finished `release` profile [optimized] target(s)"))
        .success();

    let path = project.release_wasm("foo");

    validate_component(&path)?;

    let wasm = fs::read(&path)
        .with_context(|| format!("failed to read wasm file `{path}`", path = path.display()))?;

    let metadata = match wasm_metadata::Payload::from_binary(&wasm)? {
        wasm_metadata::Payload::Component { metadata, .. } => metadata,
        wasm_metadata::Payload::Module(_) => unreachable!("found a wasm module"),
    };

    assert_eq!(
        &metadata.name.as_ref().expect("missing name").to_string(),
        name
    );
    assert_eq!(
        &metadata
            .authors
            .as_ref()
            .expect("missing authors")
            .to_string(),
        authors
    );
    assert_eq!(
        &metadata
            .description
            .as_ref()
            .expect("missing description")
            .to_string(),
        description
    );
    assert_eq!(
        &metadata
            .licenses
            .as_ref()
            .expect("missing licenses")
            .to_string(),
        license
    );
    assert_eq!(
        &metadata
            .source
            .as_ref()
            .expect("missing source")
            .to_string(),
        repository
    );
    assert_eq!(
        &metadata
            .homepage
            .as_ref()
            .expect("missing homepage")
            .to_string(),
        homepage
    );
    assert_eq!(
        &metadata
            .version
            .as_ref()
            .expect("missing version")
            .to_string(),
        version
    );

    Ok(())
}
