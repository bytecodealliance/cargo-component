<div align="center">
  <h1><code>cargo component</code></h1>

<strong>A <a href="https://bytecodealliance.org/">Bytecode Alliance</a> project</strong>

  <p>
    <strong>A cargo subcommand for building WebAssembly components according to the <a href="https://github.com/WebAssembly/component-model/">component model proposal</a>.</strong>
  </p>

  <p>
    <a href="https://crates.io/crates/cargo-component"><img src="https://img.shields.io/crates/v/cargo-component.svg?style=flat-square" alt="Crates.io version" /></a>
    <a href="https://crates.io/crates/cargo-component"><img src="https://img.shields.io/crates/d/cargo-component.svg?style=flat-square" alt="Download" /></a>
    <a href="https://bytecodealliance.github.io/cargo-component/"><img src="https://img.shields.io/badge/docs-latest-blue.svg?style=flat-square" alt="docs.rs docs" /></a>
  </p>
</div>

## Installation

To install the `cargo component` subcommand, first you'll want to install
[the latest stable Rust](https://www.rust-lang.org/tools/install) and then
you'll execute to  install the subcommand from the root of this repository:

```
cargo install --path .
```

The [currently published crate](https://crates.io/crates/cargo-component)
on crates.io is a nonfunctional placeholder and these instructions will be
updated to install the crates.io package once a proper release is made.

## Usage

The `cargo component` subcommand has some analogous commands to cargo itself:

* `cargo component new` — creates a new WebAssembly component Rust project.
* `cargo component build` — builds a WebAssembly component from a Rust project
  using the `wasm32-unknown-unknown` target by default.
* (_coming soon_) `cargo component metadata` — prints package metadata as `cargo metadata` would,
  except it also includes the metadata of generated bindings.
* (_coming soon_) `cargo component check` — checks the local package and all of its dependencies
  (including generated bindings) for errors.

More commands will be added over time.

## Specifying component dependencies

Component dependencies are interfaces defined in [wit](https://github.com/bytecodealliance/wit-bindgen)
that are listed in a special section in the project's `Cargo.toml` file: 

```toml
[package.metadata.component.dependencies]
```

Dependencies are specified much like normal path dependencies in `Cargo.toml`:

```toml
binding-name = { version = "0.1.0", path = "path/to/interface.wit" }
```

By default, dependencies specified this way are for _imported_ interfaces.

To specify an _exported_ interface, use the `export` key set to `true`:

```toml
binding-name = { version = "0.1.0", path = "path/to/interface.wit", export = true }
```

To export a _default_ interface (i.e. one where the interface's functions
are directly exported by the component itself), omit the `version` key:

```toml
binding-name = { path = "path/to/interface.wit", export = true }
```

Only one _default_ interface may be specified.

**Support for specifying version dependencies (e.g. `dep = "0.1.0"`) from a component registry will eventually be supported.**

## Using `cargo component` with `rust-analyzer`  (_coming soon_)

[rust-analyzer](https://github.com/rust-analyzer/rust-analyzer) is an extremely
useful tool for analyzing Rust code and is used in many different editors to provide
code completion and other features.

rust-analyzer depends on `cargo metadata` and `cargo check` to discover workspace
information and to check for errors.

Because `cargo component` generates code for dependencies that `cargo` itself is
unaware of, rust-analyzer will not detect or parse the generated bindings; additionally,
diagnostics will highlight any use of the generated bindings as errors.

To solve this problem, rust-analyzer must be configured to use the `cargo-component`
executable as the `cargo` command. By doing so, the `cargo component metadata` and
`cargo component check` subcommands will inform rust-analyzer of the generated bindings
as if they were normal crate dependencies.

To configure rust-analyzer to use the `cargo-component` executable, set the
`rust-analyzer.server.extraEnv` setting to the following:

```json
"rust-analyzer.server.extraEnv": { "CARGO": "cargo-component" }
```

For Visual Studio Code, this can be done in a `.vscode/settings.json` file.

Please check the documentation for rust-analyzer regarding how to set settings for other IDEs.

## Contributing to `cargo component`

`cargo component` is a [Bytecode Alliance](https://bytecodealliance.org/) project, and follows
the Bytecode Alliance's [Code of Conduct](CODE_OF_CONDUCT.md) and
[Organizational Code of Conduct](ORG_CODE_OF_CONDUCT.md).

### Prerequisites

1. The `cargo component` subcommand is written in Rust, so you'll want
  [Rust installed](https://www.rust-lang.org/tools/install) first.

### Getting the code

You'll clone the code via `git`:

```
git clone https://github.com/bytecodealliance/cargo-component
```

### Testing changes

We'd like tests ideally to be written for all changes. Test can be run via:

```
cargo test
```

You'll be adding tests primarily to the `tests/` directory.

### Submitting changes

Changes to `cargo component` are managed through pull requests (PRs). Everyone is
welcome to submit a pull request! We'll try to get to reviewing it or
responding to it in at most a few days.

### Code formatting

Code is required to be formatted with the current Rust stable's `cargo fmt`
command. This is checked on CI.

### Continuous integration (_coming soon_)

The CI for the `cargo component` repository is relatively significant. It tests
changes on Windows, macOS, and Linux. It also performs a "dry run" of the
release process to ensure that release binaries can be built and are ready to be
published.

### Publishing a new version (_coming soon_)

Publication of this crate is entirely automated via CI. A publish happens
whenever a tag is pushed to the repository, so to publish a new version you'll
want to make a PR that bumps the version numbers (see the `bump.rs` scripts in
the root of the repository), merge the PR, then tag the PR and push the tag.
That should trigger all that's necessary to publish all the crates and binaries
to crates.io.
