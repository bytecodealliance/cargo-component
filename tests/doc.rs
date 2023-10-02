use crate::support::*;
use anyhow::{Context, Result};
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;

mod support;

#[test]
fn it_documents() -> Result<()> {
    let project = Project::new("foo")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    project
        .cargo_component("doc --color=never")
        .assert()
        .stderr(contains("Finished dev [unoptimized + debuginfo] target(s)"))
        .success();

    let doc = project.build_dir().join("doc");

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
