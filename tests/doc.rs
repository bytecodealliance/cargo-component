use crate::support::*;
use anyhow::{Context, Result};
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;

mod support;

#[test]
fn help() {
    for arg in ["help doc", "doc -h", "doc --help"] {
        cargo_component(arg)
            .assert()
            .stdout(contains(
                "Generate API documentation for a WebAssembly component API",
            ))
            .success();
    }
}

#[test]
fn it_documents() -> Result<()> {
    let project = Project::new("foo")?;
    project
        .cargo_component("doc")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();

    let doc = project.build_dir().join("wasm32-wasi").join("doc");

    let path = doc.join("src").join("foo").join("lib.rs.html");
    let content = fs::read(&path).with_context(|| {
        format!(
            "failed to read generated doc file `{path}`",
            path = path.display()
        )
    })?;
    assert!(std::str::from_utf8(&content)?.contains("Say hello!"));

    Ok(())
}
