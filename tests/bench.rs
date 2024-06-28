use std::fs;

use anyhow::Result;
use assert_cmd::prelude::*;
use predicates::str::contains;

use crate::support::*;

mod support;

#[test]
#[cfg_attr(
    windows,
    ignore = "test is currently failing in ci and needs to be debugged"
)]
fn it_runs_bench_with_basic_component() -> Result<()> {
    let project = Project::new("foo", true)?;

    fs::write(
        project.root().join("wit/world.wit"),
        "
package my:fibonacci;

interface types {
    type size = u32;
}

world generator {
    use types.{size};
    export fibonacci: func(input: size) -> u32;
}",
    )?;

    fs::write(
        project.root().join("src/lib.rs"),
        r#"
#![feature(test)]

#[allow(warnings)]
mod bindings;

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

bindings::export!(Component with_types_in bindings);

#[bench]
fn bench_recursive_fibonacci(b: &mut Bencher) {
    b.iter(|| {
        (0..5).map(fibonacci).collect::<Vec<u32>>()
    })
}"#,
    )?;

    project
        .cargo_component(["bench"])
        .env("RUSTUP_TOOLCHAIN", "nightly")
        .assert()
        .stdout(contains("test bench_recursive_fibonacci ..."))
        .stdout(contains("test result: ok."))
        .success();

    Ok(())
}
