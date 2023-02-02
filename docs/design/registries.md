# Component Registries

This document describes the design of using component registries from 
`cargo-component`.

## Motivation

Currently, `cargo-component` only supports referencing interface definitions 
from local files.

It also lacks support for directly referencing other components to use as a 
dependency.

In the near future, _component registries_ will exist that are designed 
specifically to support the WebAssembly component model.

Component authoring tooling like `cargo-component` should be able to easily 
reference packages from a component registry to facilitate the development of 
components.

## Goals

The design follows these goals:

* **Follow `cargo` conventions**

  * The design should be familiar (enough) to Rust developers given that using 
    dependencies from `crates.io` is universally understood.

* **Don't force wit on users**

  * The design should enable users to use components from a registry without
    forcing them to immediately learn wit.

* **Fully support the component model**

  * The design should enable users to leverage wit when authoring their 
    component and make it easy to reference registry packages in wit.

## Registry package types

This design follows the [SIG Registry glossary](https://github.com/bytecodealliance/SIG-Registries/blob/main/glossary.md)
definition of package types.

`cargo-component` will support expressing dependencies on the following package 
types:

* `wit` - a binary-encoded wit document defining any number of interfaces and 
  worlds; a wit package describes component model _types_.

* `component` - a binary-encoded WebAssembly component; a component package 
  contains an _implementation_ (i.e. code) in addition to describing component 
  model _types_.

Note that both package types use the binary AST of the component model.

## Design

With the goal being to feel familiar to Rust developers, `Cargo.toml` will be 
used to express dependencies on packages from a component registry.

However, as to not recreate all of the expressivity of wit itself using TOML, 
what can be accomplished with `Cargo.toml` alone will be limited.

When more expressivity is required, a wit document can be used to directly describe the authored component's type.

### `Cargo.toml` syntax

This design proposes the following tables in `Cargo.toml`:

* `[package.metadata.component]`

  This table contains information about the component being authored. The 
  supported fields are:

  * `package` - the name of the component package to use when publishing to a 
    component registry; required to publish.

  * `targets` - a target world for the component being authored; this causes 
    bindings to be generated for the specified world's imports and exports.

  See below for more information on the `targets` field.

* `[package.metadata.component.dependencies]`

  This table contains information about component dependencies.
  
  The dependencies may come from a component registry or from local wit 
  documents and components.

  Specifying dependencies from a registry is done in the general form of:

  ```toml
  name = { package = "<package-id>", version = "<version>", registry = "<registry>" }
  ```

  The `registry` field is optional and defaults to `default` (see the `[package.metadata.component.registries]`
  table below).

  As packages in a component registry are namespaced (unlike Rust crate 
  registries), the shorthand form differs from what is supported by `cargo`:

  ```toml
  name = "<package-id>:<version>"
  ```

  Which is equivalent to:

  ```toml
  name = { package = "<package-id>", version = "<version>" }
  ```

  Local wit documents and components are specified using the `path` field:

  ```toml
  name = { path = "<path>" }
  ```

  In the future, it may be possible to specify a path to a directory containing 
  a `Cargo.toml` that itself defines a component and treat it as a component 
  package dependency.

  The names of dependencies correspond to "packages" in a `wit` document and 
  may also influence the names of imports in the component being authored ([see dependency behavior](#dependency-behavior)).

* `[package.metadata.component.registries]`

  This table is entirely optional and is a map of friendly names to registry 
  URL (the format of the URL is dependent upon the underlying 
  registry client implementation):

  ```toml
  name = "<registry-url>"
  ```

  This is the shorthand form of:

  ```toml
  name = { url = "<registry-url>" }
  ```

  Dependencies may specify a specific registry to use by specifying the `registry` field:

  ```toml
  [package.metadata.component.dependencies]
  foo = { package = "ns/foo", version = "0.1.0", registry = "my-registry" }
  ```

  The `default` name is used when no `registry` field is specified in a
  dependency. Therefore, specifying a registry with name `default` will 
  override the built-in default in `cargo-component` (expected to be a 
  future Bytecode Alliance component registry instance).

  A local filesystem registry (i.e. a directory containing vendored packages 
  and their package logs) may be specified using the `path` field of a registry 
  entry:

  ```toml
  name = { path = "<path>" }
  ```

  Local filesystem registries will be the first supported registry 
  implementation in `cargo-component` while the implementation of 
  component registries is still in progress.

  It should be possible to specify the registries at the workspace root `Cargo.toml`
  as well, allowing for a single set of registries to be used across a 
  workspace.

#### The `targets` field

The `targets` field in `[package.metadata.component]` is used to specify a 
world the component is intended to target.

Specifying the target is done in the general form of:

```toml
targets = { dependency = "<dependency>", document = "<document>", world = "<world>" }
```

If _only_ the `dependency` field is specified, the dependency must be a 
component and it signifies that the component being authored will target the 
same world as the dependency.

Otherwise, the specified dependency must be a wit package and the `document` 
field is required. The `world` field remains optional; if not present the 
default world of the specified document is used.

The `targets` field has a shorthand form of `"<dependency>[.<document>[.<world>]]"`.

For example, the following:

```toml
targets = "wasi.cli.command"
```

is equivalent to:

```toml
targets = { dependency = "wasi", document = "cli", world = "command" }
```

Components may target a local wit package by specifying the `path` field:

```toml
targets = { path = "<path>", world = "<world>" }
```

The path will be parsed as a wit document and it may reference external packages
as specified in the `package.metadata.component.dependencies` table. If `world` is omitted in this form, the document must define a default world.

Components that target a local wit document _will not_ automatically import
from component package dependencies; it is expected that the document will
fully describe the imports and exports of the component.

Components that target a dependency's world will _additionally import_ from
any component package dependencies in addition to the imports of the targeted world

## Examples

### Targeting a world from a dependency

An example `Cargo.toml`:

```toml
[package]
name = "my-component"
version = "1.2.3"

[dependencies]
# Rust crate dependencies here

[package.metadata.component]
package = "my-org/my-component"
targets = "wasi.cli.command"

[package.metadata.component.dependencies]
wasi = "webassembly/wasi:1.2.3"
```

The above might be the future default output from `cargo component new`.

Here the component being authored _may_ import what is expected to be imported 
by the `command` world and _must_ export what is expected to be exported 
by the `command` world via the generated bindings.

In theory, the authored component could then simply run in any host that 
supports the `wasi.cli.command` world (e.g. a future Wasmtime CLI).

### Targeting a dependency's world and using other components from a registry

An example `Cargo.toml`:

```toml
[package]
name = "my-component"
version = "1.2.3"

[dependencies]
# Rust crate dependencies here

[package.metadata.component]
package = "my-org/my-component"
targets = "wasi.cli.command"

[package.metadata.component.dependencies]
wasi = "webassembly/wasi:1.2.3"
regex = "fancy-components/regex:1.0.0"
transcoder = "fancy-components/transcoder:1.0.0"
```

In this example, the component still targets the `wasi.cli.command` world as 
above.

However, it will also import an _instance_ named `regex` and an instance named 
`transcoder` that export the functions directly exported by the `regex` and 
`transcoder` components, respectively.

The component produced by `cargo-component` will contain URL references to the 
component dependencies and these serve as hints for later composition tooling 
to instantiate those particular components and link them with this one by 
default. As they are instance imports, the authored component may still be 
linked against alternative implementations provided they implement the expected 
interfaces according to component model subtyping rules.

### Defining a custom world for a component

An example `Cargo.toml`:

```toml
[package]
name = "my-component"
version = "1.2.3"

[dependencies]
# Rust crate dependencies here

[package.metadata.component]
package = "my-org/my-component"
targets = { path = "component.wit" }

[package.metadata.component.dependencies]
wasi = "webassembly/wasi:1.2.3"
regex = "fancy-components/regex:1.0.0"
transcoder = "fancy-components/transcoder:1.0.0"
```

An example `component.wit`:

```wit
default world my-component {
  include wasi.cli.command # a proposed syntax for including a world in another
  
  import regex: regex.root
  import transcoder: transcoder.root
  
  export parse: func(x: string) -> result<_, string>
}
```

Because the `targets` specifies a local wit document, the dependencies are 
interpreted as wit external packages only; `regex` and `transcoder` are not 
imported automatically as in the previous example.

The resulting type for this component will the same as the previous example, 
but it will also export a `parse` function of type `(string) -> result<_, string>`
as specified in the world.

The wit document package names of `wasi`, `regex`, and `transcoder` map 
directly to the names of the dependencies in `[package.metadata.component.dependencies]`.

Referencing a package in the wit document that is not defined in the `[package.metadata.component.dependencies]`
table is an error.

### Exporting only functions from a component

An example `Cargo.toml`:

```toml
[package]
name = "my-component"
version = "1.2.3"

[dependencies]
# Rust crate dependencies here

[package.metadata.component]
package = "my-org/my-component"
targets = { path = "component.wit" }
```

An example `component.wit`:

```wit
default world my-component {
  export greet: func(name: string) -> string
}
```

Here this component has no component registry dependencies and only exports a 
single function named `greet` with type `(string) -> string`.
