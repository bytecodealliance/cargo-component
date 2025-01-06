use std::fs;

use anyhow::{Context, Result};
use assert_cmd::prelude::*;
use futures::stream::TryStreamExt;
use predicates::str::contains;
use toml_edit::{value, Array};
use wasm_metadata::LinkType;
use wasm_pkg_client::Client;

use crate::support::*;

mod support;

#[test]
fn help() {
    for arg in ["help publish", "publish -h", "publish --help"] {
        cargo_component(arg.split_whitespace())
            .assert()
            .stdout(contains("Publish a package to a registry"))
            .success();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_component() -> Result<()> {
    let (server, config, _) = spawn_server(Vec::<String>::new()).await?;

    publish_wit(
        config,
        "test:world",
        "1.0.0",
        r#"package test:%world@1.0.0;
world foo {
    import foo: func() -> string;
    export bar: func() -> string;
}"#,
    )
    .await?;

    let project = server.project(
        "foo",
        true,
        ["--namespace", "test", "--target", "test:world"],
    )?;

    // Ensure there's a using declaration in the generated source
    let source = fs::read_to_string(project.root().join("src/lib.rs"))?;
    assert!(source.contains("use bindings::Guest;"));

    project
        .cargo_component(["publish"])
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    let path = project.root().join("wkg.lock");
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read lock file `{path}`", path = path.display()))?;

    assert!(contents.contains("name = \"test:world\""));
    assert!(contents.contains("version = \"1.0.0\""));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_a_dependency() -> Result<()> {
    let (server, config, _) = spawn_server(Vec::<String>::new()).await?;

    publish_wit(
        config,
        "test:world",
        "1.0.0",
        r#"package test:%world@1.0.0;
world foo {
    export bar: func() -> string;
}"#,
    )
    .await?;

    let project = server.project(
        "foo",
        true,
        ["--namespace", "test", "--target", "test:world/foo"],
    )?;

    project
        .cargo_component(["publish"])
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    let project = server.project(
        "bar",
        true,
        ["--namespace", "test", "--target", "test:world"],
    )?;
    project
        .cargo_component(["add", "test:foo"])
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
        .cargo_component(["publish"])
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:bar` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("bar"))?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn it_publishes_with_registry_metadata() -> Result<()> {
    let (server, config, _) = spawn_server(Vec::<String>::new()).await?;

    let authors = ["Jane Doe <jane@example.com>"];
    let categories = ["wasm"];
    let description = "A test package";
    let license = "Apache-2.0";
    let documentation = "https://example.com/docs";
    let homepage = "https://example.com/home";
    let repository = "https://example.com/repo";

    let project = server.project("foo", true, ["--namespace", "test"])?;
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
        .cargo_component(["publish"])
        .env("CARGO_COMPONENT_PUBLISH_KEY", test_signing_key())
        .assert()
        .stderr(contains("Published package `test:foo` v0.1.0"))
        .success();

    validate_component(&project.release_wasm("foo"))?;

    let client = Client::new(config);
    let package_ref = "test:foo".parse().unwrap();
    let release = client
        .get_release(&package_ref, &"0.1.0".parse().unwrap())
        .await?;
    let stream = client.stream_content(&package_ref, &release).await?;

    let bytes = stream.map_ok(Vec::from).try_concat().await?;

    let metadata = wasm_metadata::RegistryMetadata::from_wasm(&bytes)
        .context("failed to parse registry metadata from data")?
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
