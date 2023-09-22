use crate::support::*;
use anyhow::Result;
use assert_cmd::prelude::*;
use std::fs;

mod support;

#[test]
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
use std::mem::replace;
use test::Bencher;

struct Component;

fn fibonacci(n: Size) -> u32 {
    if n < 2 {
        1
    } else {
        fibonacci(n - 1) + fibonacci(n - 2)
    }
}

struct Fibonacci {
    curr: u32,
    next: u32,
}

impl Iterator for Fibonacci {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        let new_next = self.curr + self.next;
        let new_curr = replace(&mut self.next, new_next);

        Some(replace(&mut self.curr, new_curr))
    }
}

fn fibonacci_sequence() -> Fibonacci {
    Fibonacci { curr: 1, next: 1 }
}

impl Guest for Component {
    fn fibonacci(size: Size) -> u32 {
        fibonacci(size)
    }
}

#[bench]
fn bench_recursive_fibonacci(b: &mut Bencher) {
    b.iter(|| {
        (0..20).map(fibonacci).collect::<Vec<u32>>()
    })
}
"#,
    )?;

    project.cargo_component("bench").assert().success();

    Ok(())
}
