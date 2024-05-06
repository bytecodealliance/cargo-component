use crate::support::*;
use anyhow::{Context, Result};
use assert_cmd::prelude::*;
use predicates::str::contains;
use semver::Version;
use std::{fs, rc::Rc};
use tempfile::TempDir;
use toml_edit::{value, Array};
use warg_client::Client;
use warg_protocol::registry::PackageName;
use wasm_metadata::LinkType;

mod support;

#[test]
fn help() {
    for arg in ["help publish", "publish -h", "publish --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains("Publish a package to a registry"))
            .success();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_component() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    publish_wit(
        &config,
        "test:world",
        "1.0.0",
        r#"package test:%world@1.0.0;
world foo {
    import foo: func() -> string;
    export bar: func() -> string;
}"#,
        true,
    )
    .await?;

    let project = Project::with_dir(dir.clone(), "foo", "--namespace test --target test:world")?;

    // Ensure there's a using declaration in the generated source
    let source = fs::read_to_string(project.root().join("src/lib.rs"))?;
    assert!(source.contains("use bindings::Guest;"));

    project
        .cargo_component("publish --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    let path = project.root().join("Cargo-component.lock");
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read lock file `{path}`", path = path.display()))?;

    assert!(contents.contains("name = \"test:world\""));
    assert!(contents.contains("version = \"1.0.0\""));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_fails_if_package_does_not_exist() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    publish_wit(
        &config,
        "test:world",
        "1.0.0",
        r#"package test:%world@1.0.0;
world foo {
    import foo: func() -> string;
    export bar: func() -> string;
}"#,
        true,
    )
    .await?;

    let project = Project::with_dir(dir.clone(), "foo", "--namespace test --target test:world")?;

    project
        .cargo_component("publish")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains(
            "package `test:foo` must be initialized before publishing",
        ))
        .failure();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_dependency() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    publish_wit(
        &config,
        "test:world",
        "1.0.0",
        r#"package test:%world@1.0.0;
world foo {
    export bar: func() -> string;
}"#,
        true,
    )
    .await?;

    let project = Project::with_dir(
        dir.clone(),
        "foo",
        "--namespace test --target test:world/foo",
    )?;

    project
        .cargo_component("publish --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    let project = Project::with_dir(dir.clone(), "bar", "--namespace test --target test:world")?;

    project
        .cargo_component("add test:foo")
        .assert()
        .stderr(contains("Added dependency `test:foo` with version `0.1.0`"))
        .success();

    let source = r#"
#[allow(warnings)]
mod bindings;
use bindings::Guest;
struct Component;
impl Guest for Component {
    fn bar() -> String {
        bindings::test_foo::bar()
    }
}

bindings::export!(Component with_types_in bindings);
"#;

    fs::write(project.root().join("src/lib.rs"), source)?;

    project
        .cargo_component("publish --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("bar"))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_with_registry_metadata() -> Result<()> {
    let dir = Rc::new(TempDir::new()?);
    let (_server, config) = spawn_server(dir.path()).await?;
    config.write_to_file(&dir.path().join("warg-config.json"))?;

    let authors = ["Jane Doe <jane@example.com>"];
    let categories = ["wasm"];
    let description = "A test package";
    let license = "Apache-2.0";
    let documentation = "https://example.com/docs";
    let homepage = "https://example.com/home";
    let repository = "https://example.com/repo";

    let project = Project::with_dir(dir.clone(), "foo", "--namespace test")?;
    project.update_manifest(|mut doc| {
        let package = &mut doc["package"];
        package["authors"] = value(Array::from_iter(authors));
        package["categories"] = value(Array::from_iter(categories));
        package["description"] = value(description);
        package["license"] = value(license);
        package["documentation"] = value(documentation);
        package["homepage"] = value(homepage);
        package["repository"] = value(repository);
        Ok(doc)
    })?;

    project
        .cargo_component("publish --init")
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    let client = Client::new_with_config(None, &config, None)?;
    let download = client
        .download_exact(&PackageName::new("test:foo")?, &Version::parse("0.1.0")?)
        .await?;

    let bytes = fs::read(&download.path).with_context(|| {
        format!(
            "failed to read downloaded package `{path}`",
            path = download.path.display()
        )
    })?;

    let metadata = wasm_metadata::RegistryMetadata::from_wasm(&bytes)
        .with_context(|| {
            format!(
                "failed to parse registry metadata from `{path}`",
                path = download.path.display()
            )
        })?
        .expect("missing registry metadata");

    assert_eq!(
        metadata.get_authors().expect("missing authors").as_slice(),
        authors
    );
    assert_eq!(
        metadata
            .get_categories()
            .expect("missing categories")
            .as_slice(),
        categories
    );
    assert_eq!(
        metadata.get_description().expect("missing description"),
        description
    );
    assert_eq!(metadata.get_license().expect("missing license"), license);

    let links = metadata.get_links().expect("missing links");
    assert_eq!(links.len(), 3);

    assert_eq!(
        links
            .iter()
            .find(|link| link.ty == LinkType::Documentation)
            .expect("missing documentation")
            .value,
        documentation
    );
    assert_eq!(
        links
            .iter()
            .find(|link| link.ty == LinkType::Homepage)
            .expect("missing homepage")
            .value,
        homepage
    );
    assert_eq!(
        links
            .iter()
            .find(|link| link.ty == LinkType::Repository)
            .expect("missing repository")
            .value,
        repository
    );

    Ok(())
}
