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
2. A [protobuf compiler](http://google.github.io/proto-lens/installing-protoc.html)
  of version 3.15 or greater is also required for registry support.

## Installation

To install the `cargo component` subcommand, run the following command:

```
cargo install --git https://github.com/bytecodealliance/cargo-component --locked
```

The [currently published crate](https://crates.io/crates/cargo-component)
on crates.io is a nonfunctional placeholder and these instructions will be
updated to install the crates.io package once a proper release is made.

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
* reference it like you would an external crate (via `<name>::...`) in
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

Unlike `wit-bindgen`, `cargo component` doesn't use procedural macros
or a `build.rs` file to generate bindings. Instead, it generates them
into external crates that are automatically provided to the Rust
compiler when building your component's project.

This approach does come with some downsides, however. Commands like
`cargo metadata` and `cargo check` used by many tools (e.g.
`rust-analyzer`) simply don't work because they aren't aware of the
generated bindings. That is why replacement commands such as
`cargo component metadata` and `cargo component check` exist.

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

Use `cargo component new --lib <name>` to create a new reactor component.

This will create a `wit/world.wit` file describing the world that the
component will target:

```wit
default world component {
  /// Say hello!
  export hello-world: func() -> string
}
```

The component will export a `hello-world` function returning a string.

The implementation of the component will be in `src/lib.rs`:

```rust
struct Component;

impl bindings::Component for Component {
    fn hello_world() -> String {
        "Hello, World!".to_string()
    }
}

bindings::export!(Component);
```

Here `bindings` is the bindings crate that `cargo component` generated for you.

The `export!` macro informs the bindings that the `Component` type exports
all interfaces listed in `Cargo.toml`.

## Usage

The `cargo component` subcommand has some analogous commands to cargo itself:

* `cargo component new` — creates a new WebAssembly component Rust project.
* `cargo component add` — adds a component interface dependency to a cargo 
  manifest file.
* `cargo component build` — builds a WebAssembly component from a Rust project
  using the `wasm32-wasi` target by default.
* `cargo component doc` — generates API documentation for a WebAssembly 
  component from a Rust project.
* `cargo component metadata` — prints package metadata as `cargo metadata` 
  would, except it also includes the metadata of generated bindings.
* `cargo component check` — checks the local package and all of its dependencies
  (including generated bindings) for errors.
* `cargo component clippy` — same as `cargo clippy` but also checks generated 
  bindings.
* `cargo component update` — same as `cargo update` but also updates the 
  dependencies in the component lock file.
* `cargo component publish` - publishes a WebAssembly component to a [warg](https://warg.io/)
  component registry.
* `cargo component key` - manages signing keys for publishing WebAssembly 
  components.

More commands will be added over time.

## Using `rust-analyzer`

[rust-analyzer](https://github.com/rust-analyzer/rust-analyzer) is an extremely
useful tool for analyzing Rust code and is used in many different editors to 
provide code completion and other features.

rust-analyzer depends on `cargo metadata` and `cargo check` to discover 
workspace information and to check for errors.

Because `cargo component` generates code for dependencies that `cargo` itself is
unaware of, rust-analyzer will not detect or parse the generated bindings; 
additionally, diagnostics will highlight any use of the generated bindings as 
errors.

To solve this problem, rust-analyzer must be configured to use the 
`cargo-component` executable as the `cargo` command. By doing so, the `cargo 
component metadata` and `cargo component check` subcommands will inform 
rust-analyzer of the generated bindings as if they were normal crate 
dependencies.

To configure rust-analyzer to use the `cargo-component` executable, set the
`rust-analyzer.server.extraEnv` setting to the following:

```json
"rust-analyzer.server.extraEnv": { "CARGO": "cargo-component" }
```

By default, `cargo component new` will configure Visual Studio Code to use 
`cargo component` by creating a `.vscode/settings.json` file for you. To 
prevent this, pass `--editor none` to `cargo component new`.

Please check the documentation for rust-analyzer regarding how to set settings 
for other IDEs.

## Contributing to `cargo component`

`cargo component` is a [Bytecode Alliance](https://bytecodealliance.org/) 
project, and follows
the Bytecode Alliance's [Code of Conduct](CODE_OF_CONDUCT.md) and
[Organizational Code of Conduct](ORG_CODE_OF_CONDUCT.md).

### Getting the Code

You'll clone the code via `git`:

```
git clone https://github.com/bytecodealliance/cargo-component
```

### Testing Changes

To run tests, a [warg](https://warg.io/) registry server is required.

To install `warg-server` locally, run:

```
cargo install --git https://github.com/bytecodealliance/registry warg-server
```

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

It also performs a "dry run" of the release process to ensure that release 
binaries can be built and are ready to be published (_coming soon_).

### Publishing (_coming soon_)

Publication of this crate is entirely automated via CI. A publish happens
whenever a tag is pushed to the repository, so to publish a new version you'll
want to make a PR that bumps the version numbers (see the `bump.rs` scripts in
the root of the repository), merge the PR, then tag the PR and push the tag.
That should trigger all that's necessary to publish all the crates and binaries
to crates.io.
