<div align="center">
  <h1><code>cargo component</code></h1>

<strong>A <a href="https://bytecodealliance.org/">Bytecode Alliance</a> project</strong>

  <p>
    <strong>A cargo subcommand for building WebAssembly components according to the <a href="https://github.com/WebAssembly/component-model/">component model proposal</a>.</strong>
  </p>

  <p>
    <a href="https://github.com/bytecodealliance/cargo-component/actions?query=workflow%3ACI"><img src="https://github.com/bytecodealliance/cargo-component/workflows/CI/badge.svg" alt="build status" /></a>
    <a href="https://crates.io/crates/cargo-component"><img src="https://img.shields.io/crates/v/cargo-component.svg?style=flat-square" alt="Crates.io version" /></a>
    <a href="https://crates.io/crates/cargo-component"><img src="https://img.shields.io/crates/d/cargo-component.svg?style=flat-square" alt="Download" /></a>
    <a href="https://bytecodealliance.github.io/cargo-component/"><img src="https://img.shields.io/badge/docs-latest-blue.svg?style=flat-square" alt="docs.rs docs" /></a>
  </p>
</div>

## Overview

`cargo component` is a `cargo` subcommand for creating [WebAssembly components](https://github.com/WebAssembly/component-model)
using Rust as the component's implementation language.

### Notice

`cargo component` is considered to be experimental and is _not_ currently
stable in terms of the code it supports building.

Until the component model stabilizes, upgrading to a newer `cargo component`
may cause build errors for existing component projects.

## Requirements

1. The `cargo component` subcommand is written in Rust, so you'll want the
  [latest stable Rust installed](https://www.rust-lang.org/tools/install).

## Installation

To install the `cargo component` subcommand, run the following command:

```
cargo install cargo-component
```

## Motivation

Today, developers that target WebAssembly typically compile a monolithic
program written in a single source language to a WebAssembly module. The
WebAssembly module can then be used in all sorts of places: from web
browsers to cloud compute platforms. WebAssembly was intentionally designed
to provide the portability and security properties required for such
environments.

However, WebAssembly modules are not easily _composed_ with other modules
into a single program or service. WebAssembly only has a few primitive
value types (integer and floating point types) and those are inadequate
to describe the complex types that developers would desire to exchange
between modules.

To make things even more challenging, WebAssembly modules typically define
their own local linear memories, meaning one module can't access the
(conceptual) _address space_ of another. Something must sit between the
two modules to facilitate communication when pointers are passed around.

While it is possible to solve these challenges with the existing
WebAssembly standard, doing so is burdensome, error-prone, and requires
foreknowledge of how the WebAssembly modules are implemented.

## WebAssembly Component Model

The WebAssembly component model proposal provides a way to
simplify the process of building WebAssembly applications and services
out of reusable pieces of functionality using a variety of source
languages, all while still maintaining the portability and
security properties of WebAssembly.

At its most fundamental level, WebAssembly components may be used to
wrap a WebAssembly module in a way that describes how its _interface_,
a set of functions using complex value types (e.g. strings, variants,
records, lists, etc.), is translated to and from the lower-level
representation required of the WebAssembly module.

This enables WebAssembly runtimes to know specifically how they must
facilitate the exchange of data between the discrete linear memories
of components, eliminating the need for developers to do so by hand.

Additionally, components can describe their dependencies in a way
that modules simply cannot today; they can even control how their
dependencies are _instantiated_, enabling a component to
_virtualize_ functionality needed by a dependency. And because
different components might have a shared dependency, hosts may even
share the same implementation of that dependency to save on host
memory usage.

## Cargo Component

A primary goal of `cargo component` is to try to imagine what
first-class support for WebAssembly components might look like for Rust.

That means being able to reference WebAssembly components via
`Cargo.toml` and have WebAssembly component dependencies used in the
same way as Rust crate dependencies:

* add a dependency on a WebAssembly component to `Cargo.toml`
* reference it like you would an external crate (via `bindings::<name>::...`) in
  your source code
* build using `cargo component build` and out pops your component!

To be able to use a WebAssembly component from any particular
programming language, _bindings_ must be created by translating
a WebAssembly component's _interface_ to a representation that
a specific programming language can understand.

Tools like [`wit-bindgen`](https://github.com/bytecodealliance/wit-bindgen)
exist to generate those bindings for different languages,
including Rust.

`wit-bindgen` even provides procedural macros to generate the
bindings "inline" with the component's source code.

Like `wit-bindgen`, `cargo component` uses a procedural macro to generate
bindings. However, bindings are generated based on the resolved dependencies
from `Cargo.toml` rather than parsing a local definition of the component's
interface.

The hope is that one day (in the not too distant future...) that
WebAssembly components might become an important part of the Rust
ecosystem such that `cargo` itself might support them.

Until that time, there's `cargo component`!

## Status

A quick note on the implementation status of the component model
proposal.

At this time of this writing, no WebAssembly runtimes have fully
implemented the component model proposal.

[Wasmtime](https://github.com/bytecodealliance/wasmtime)
has implementation efforts underway to support it, but it's still a
_work-in-progress_.

Until runtime support grows and additional tools are implemented
for linking components together, the usefulness of `cargo component`
today is effectively limited to creating components that runtime
and tooling developers can use to test their implementations.

## WASI Support

Currently `cargo component` targets `wasm32-wasi` by default.

As this target is for a _preview1_ release of WASI, the WebAssembly module
produced by the Rust compiler must be adapted to the _preview2_ version of WASI
supported by the component model.

The adaptation is automatically performed when `wasm32-wasi` is targeted.

To prevent this, override the target to `wasm32-unknown-unknown` using the
`--target` option when building. This, however, will disable WASI support.

Use the _preview2_ version of [`wasi-common`][2] in your host to run components
produced by `cargo component`.

When the Rust compiler supports a [_preview2_ version of the WASI target][1],
support in `cargo component` for adapting a _preview1_ module will be removed.

[1]: https://github.com/rust-lang/compiler-team/issues/594
[2]: https://github.com/bytecodealliance/preview2-prototyping/tree/main/wasi-common

## Getting Started

Use `cargo component new --reactor <name>` to create a new reactor component.

A reactor component doesn't have a `run` (i.e. `main` in Rust) function
exported and is meant to be used as a library rather than a command that runs
and exits. Without the `--reactor` flag, `cargo component` defaults to creating
a command component.

This will create a `wit/world.wit` file describing the world that the
component will target:

```wit
package my-org:my-component

/// An example world for the component to target.
world example {
    export hello-world: func() -> string
}
```

The component will export a `hello-world` function returning a string.

The implementation of the component will be in `src/lib.rs`:

```rust
cargo_component_bindings::generate!();

use bindings::Guest;

struct Component;

impl Guest for Component {
    /// Say hello!
    fn hello_world() -> String {
        "Hello, World!".to_string()
    }
}
```

The `generate!` macro is responsible for generating the bindings to allow the
Rust code to export what is expected of the component.

It generates a Rust module named `bindings` containing the types and traits the
correspond to the world definition.

## Usage

The `cargo component` subcommand has some analogous commands to cargo itself:

* `cargo component new` — creates a new WebAssembly component Rust project.
* `cargo component add` — adds a component interface dependency to a cargo
  manifest file.
* `cargo component update` — same as `cargo update` but also updates the
  dependencies in the component lock file.
* `cargo component publish` - publishes a WebAssembly component to a [warg](https://warg.io/)
  component registry.
* `cargo component key` - manages signing keys for publishing WebAssembly
  components.

Unrecognized commands are passed through to `cargo` itself, but only after the
bindings information for component packages has been updated.

Some examples of commands that are passed directly to `cargo` are: `build`,
`check`, `doc`, `clippy` and extension commands such as `expand` from
`cargo-expand`.

Certain command line options, like `--target` and `--release`, are detected by
`cargo component` to determine what output files of a `build` command should be
componentized.

## Using `rust-analyzer`

[rust-analyzer](https://github.com/rust-analyzer/rust-analyzer) is an extremely
useful tool for analyzing Rust code and is used in many different editors to
provide code completion and other features.

rust-analyzer depends on `cargo metadata` and `cargo check` to discover
workspace information and to check for errors.

To ensure that rust-analyzer is able to discover the latest bindings
information, rust-analyzer must be configured to use `cargo component check` as
the check command.

To configure rust-analyzer to use the `cargo-component` executable, set the
`rust-analyzer.server.extraEnv` setting to the following:

```json
"rust-analyzer.check.overrideCommand": ["cargo", "component", "check", "--message-format=json"]
```

By default, `cargo component new` will configure Visual Studio Code to use
`cargo component check` by creating a `.vscode/settings.json` file for you. To
prevent this, pass `--editor none` to `cargo component new`.

Please check the documentation for rust-analyzer regarding how to set settings
for other IDEs.

## Contributing to `cargo component`

`cargo component` is a [Bytecode Alliance](https://bytecodealliance.org/)
project, and follows the Bytecode Alliance's [Code of Conduct](CODE_OF_CONDUCT.md)
and [Organizational Code of Conduct](ORG_CODE_OF_CONDUCT.md).

### Getting the Code

You'll clone the code via `git`:

```
git clone https://github.com/bytecodealliance/cargo-component
```

### Testing Changes

We'd like tests ideally to be written for all changes. Test can be run via:

```
cargo test
```

You'll be adding tests primarily to the `tests/` directory.

### Submitting Changes

Changes to `cargo component` are managed through pull requests (PRs). Everyone
is welcome to submit a pull request! We'll try to get to reviewing it or
responding to it in at most a few days.

### Code Formatting

Code is required to be formatted with the current Rust stable's `cargo fmt`
command. This is checked on CI.

### Continuous Integration

The CI for the `cargo component` repository is relatively significant. It tests
changes on Windows, macOS, and Linux.

### Publishing

Publication of this crate is entirely automated via CI. A publish happens
whenever a tag is pushed to the repository, so to publish a new version you'll
want to make a PR that bumps the version numbers (see the `ci/publish.rs` 
script), merge the PR, then tag the PR and push the tag. That should trigger 
all that's necessary to publish all the crates and binaries to crates.io.
