# The `wit` tool

## DEPRECATED. Please use the `wkg wit` subcommand in the [wkg crate](https://crates.io/crates/wkg).

A tool for creating and publishing WIT packages to a [WebAssembly component
registry](https://github.com/bytecodealliance/registry/).

WIT packages are used in the [WebAssembly Component Model](https://github.com/WebAssembly/component-model/)
for defining interfaces, types, and worlds used in WebAssembly components.

## Requirements

* The `wit` tool is written in Rust, so you'll want the [latest stable Rust
  installed](https://www.rust-lang.org/tools/install).

## Installation

To install `wit` subcommand, run the following command:

```
cargo install wit
```

## Initializing a WIT package

To initialize a new WIT package in the current directory:

```
wit init
```

This creates a `wit.toml` file with the following contents:

```toml
version = "0.1.0"

[dependencies]

[registries]
```

By default, the WIT package will not have any dependencies specified.

The registries section contains a mapping of registry names to URLs. The
intention behind explicitly supporting multiple registries is that no one
registry will be the central repository for WebAssembly components; in the
future, a federation of registries will be used for publishing and discovering
WebAssembly components.

A registry named `default` will be the registry to use when a dependency does
not explicitly specify a registry to use.

An example of setting the default registry:

```toml
[registries]
default = "https://preview-registry.bytecodealliance.org"
```

The default registry may also be set by passing the `--registry` option to the
`init` command:

```
wit init --registry https://preview-registry.bytecodealliance.org
```

## Adding a dependency

To add a dependency on another WIT package, use the `add` command:

```
wit add <PACKAGE>
```

Where `PACKAGE` is the package to add the dependency for, e.g. `wasi:cli`.

The command will contact the registry to determine the latest version of the
package, and add it as a dependency in the `wit.toml` file.

The version requirement to use may be specified with a delimited `@`:

```
wit add wasi:cli@2.0.0
```

## Building the WIT package

To build the WIT package to a binary WebAssembly file, use the `build` command:

```
wit build
```

This command will output a `.wasm` file based on the package name parsed from
the `.wit` files in the directory containing `wit.toml`.

Use the `--output` option to specify the output file name:

```
wit build --output my-package.wasm
```

## Updating dependencies

To update the dependencies of a WIT package, use the `update` command:

```
wit update
```

This command will contact the registry for the latest versions of the
dependencies specified in `wit.toml` and update the versions in the lock file,
`wit.lock`.

## Publishing the WIT package to a registry

To publish the WIT package to a registry, use the `publish` command:

```
wit publish
```

The command will publish the package to the default registry using the default
signing key.

To specify a different registry or signing key, use the `--registry` and
`--key-name` options, respectively:

```
wit publish --registry https://registry.example.com --key-name my-signing-key
```

## Managing signing keys

WebAssembly component registries accept packages based on the keys used to sign
the records being published.

The `wit` tool uses the OS-provided keyring to securely store signing keys.
Use the [`warg` CLI](https://crates.io/crates/warg-cli) to manage your signing keys.

## Contributing to `wit`

`wit` is a (future) [Bytecode Alliance](https://bytecodealliance.org/)
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
cargo test -p wit
```

You'll be adding tests primarily to the `tests/` directory.

### Submitting Changes

Changes to `wit` are managed through pull requests (PRs). Everyone
is welcome to submit a pull request! We'll try to get to reviewing it or
responding to it in at most a few days.

### Code Formatting

Code is required to be formatted with the current Rust stable's `cargo fmt`
command. This is checked on CI.

### Continuous Integration

The CI for the `wit` repository is relatively significant. It tests
changes on Windows, macOS, and Linux.

### Publishing

Publication of this crate is entirely automated via CI. A publish happens
whenever a tag is pushed to the repository, so to publish a new version you'll
want to make a PR that bumps the version numbers (see the `ci/publish.rs` 
script), merge the PR, then tag the PR and push the tag. That should trigger 
all that's necessary to publish all the crates and binaries to crates.io.
