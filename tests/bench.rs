use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;

mod support;

#[test]
#[ignore = "only works in nightly mode"]
fn it_runs_bench_with_basic_component() -> Result<()> {
    let project = Project::new("foo")?;
    project.update_manifest(|mut doc| {
        redirect_bindings_crate(&mut doc);
        Ok(doc)
    })?;

    fs::write(
        project.root().join("wit/world.wit"),
        "
package my:fibonacci

interface types {
    type size = u32
}

world generator {
    use types.{size}
    export fibonacci: func(input: size) -> u32
}",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        r#"
#![feature(test)]

cargo_component_bindings::generate!();

extern crate test;

use bindings::{Guest, Size};
use test::Bencher;

struct Component;

fn fibonacci(n: Size) -> u32 {
    if n < 2 {
        1
    } else {
        fibonacci(n - 1) + fibonacci(n - 2)
    }
}

impl Guest for Component {
    fn fibonacci(size: Size) -> u32 {
        fibonacci(size)
    }
}

#[bench]
fn bench_recursive_fibonacci(b: &mut Bencher) {
    b.iter(|| {
        (0..5).map(fibonacci).collect::<Vec<u32>>()
    })
}"#,
    )?;

    project
        .cargo_component("bench")
        .assert()
        .stdout(contains("test bench_recursive_fibonacci ..."))
        .stdout(contains("test result: ok."))
        .success();

    Ok(())
}
