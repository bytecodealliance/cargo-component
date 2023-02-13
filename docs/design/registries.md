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

* **Don't force WIT on users**

  * The design should enable users to use components from a registry without
    forcing them to immediately learn WIT.

* **Fully support the component model**

  * The design should enable users to leverage WIT when authoring their 
    component and make it easy to reference registry packages in WIT.

## Registry package types

This design follows the [SIG Registry glossary](https://github.com/bytecodealliance/SIG-Registries/blob/main/glossary.md)
definition of package types.

`cargo-component` will support expressing dependencies on the following package 
types:

* `wit` - a binary-encoded WIT document defining any number of interfaces and 
  worlds; a WIT package describes component model _types_.

* `component` - a binary-encoded WebAssembly component; a component package 
  contains an _implementation_ (i.e. code) in addition to describing component 
  model _types_.

Note that both package types use the binary AST of the component model.

## Design

With the goal being to feel familiar to Rust developers, `Cargo.toml` will be 
used to express dependencies on packages from a component registry.

However, as to not recreate all of the expressivity of WIT itself using TOML, 
what can be accomplished with `Cargo.toml` alone will be limited.

When more expressivity is required, a WIT document can be used to directly 
describe the authored component's type.

## `Cargo.toml` syntax

This design proposes the following tables in `Cargo.toml`:

* `[package.metadata.component]`

  This table contains information about the component being authored. The 
  supported fields are:

  * `package` - the name of the component package to use when publishing to a 
    component registry; required to publish.

  * `target` - a target world for the component being authored; this causes 
    bindings to be generated for the specified world's imports and exports.

    See the [target field](#the-target-field) section below for more 
    information.

* `[package.metadata.component.dependencies]`

  This table contains information about component dependencies.

  Each entry in the table must reference a WebAssembly component.

  An import will be added for each component dependency in the world that the 
  component targets.
  
  The dependencies may come from a component registry or local components.

  Specifying dependencies from a registry is done in the general form of:

  ```toml
  name = { package = "<package-id>", version = "<version>", registry = "<registry>" }
  ```

  The `registry` field is optional and defaults to `default` (see the `[package.metadata.component.registries]`
  table below).

  As packages in a component registry are namespaced (unlike Rust crate 
  registries), the shorthand form differs from what is supported by `cargo`:

  ```toml
  name = "<package-id>@<version>"
  ```

  Which is equivalent to:

  ```toml
  name = { package = "<package-id>", version = "<version>" }
  ```

  Local components are specified using the `path` field:

  ```toml
  name = { path = "<path>" }
  ```

  In the future, it may be possible to specify a path to a directory containing 
  a `Cargo.toml` that itself defines a component and treat it as a component 
  package dependency.
  
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

  Component dependencies may specify a specific registry to use by specifying 
  the `registry` field:

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

### The `target` field

The `target` field in `[package.metadata.component]` is used to specify a 
world the component is intended to target.

There are two ways to specify a target world: referencing a registry package or 
using a local WIT document.

#### Targeting a registry package

You may use a shorthand form of specifying a target as follows:

```toml
[package.metadata.component]
target = "<package-id>@<version>"
```

This is equivalent to:

```toml
[package.metadata.component]
target = { package = "<package-id>", version = "<version>" }
```

The supported fields of `target` when referencing a registry package are:

```toml
[package.metadata.component.target]
package = "<package-id>"
version = "<version>"
document = "<document>"
world = "<world>"
registry = "<registry>"
```

The `package` and `version` fields are required.

The `package`, `version`, and `registry` fields describe which package is being 
referenced. The `registry` field is optional.

The `document` field is optional and defaults to the first document in the 
package if there is exactly one document. If there are multiple documents, the 
`document` field is required.

The `world` field is optional and defaults to the default world of the 
document. If the document has no default world, then the default is the first 
world in the document if there is exactly one world. If there are multiple 
worlds in the document, the `world` field is required.

The referenced package may be either a WIT package or a component.

#### Targeting a local WIT document

Specifying a target from a local WIT document:

```toml
[package.metadata.component.target]
path = "<path>"
world = "<world>"

[package.metadata.component.target.dependencies]
"<name>" = "<package-id>@<version>" # or any of the other forms of specifying a dependency
...
```

The `path` field is required and specifies the path to the WIT document 
defining a world to target.

The `world` field is optional and defaults to the default world of the 
document. If the document has no default world, then the default is the first 
world in the document if there is exactly one world. If there are multiple 
worlds in the document, the `world` field is required.

The `[package.metadata.component.target.dependencies]` table is optional and 
defines the WIT package dependencies that may be referenced in the local WIT 
document.

A non-empty `dependencies` table is only allowed when targeting a local WIT 
document. Each dependency in the table must be a WIT package.

Referencing an external package in the WIT document that is not defined in the 
`dependencies` table is an error.

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

[package.metadata.component.target]
package = "webassembly/wasi"
version = "1.2.3"
document = "command"

[package.metadata.component.dependencies]
```

The above might be the future default output from `cargo component new`.

Here the component being authored _may_ import what is expected to be imported 
by the default `command` world and _must_ export what is expected to be 
exported by the world via the generated bindings.

In theory, the authored component could then simply run in any host that 
supports the WASI command world (e.g. a future Wasmtime CLI).

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

[package.metadata.component.target]
package = "webassembly/wasi"
version = "1.2.3"
document = "command"

[package.metadata.component.dependencies]
regex = "fancy-components/regex@1.0.0"
transcoder = "fancy-components/transcoder@1.0.0"
```

In this example, the component still targets the WASI command world as above.

However, it will also import an _instance_ named `regex` and an instance named 
`transcoder` that export the functions directly exported by the `regex` and 
`transcoder` components, respectively.

The component produced by `cargo-component` will contain URL references to the 
component dependencies and these serve as hints for later composition tooling 
to instantiate those particular components and link them with this one by 
default.

As they are instance imports, the authored component may still be 
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

[package.metadata.component.target]
path = "component.wit"

[package.metadata.component.target.dependencies]
wasi = "webassembly/wasi@1.2.3"

[package.metadata.component.dependencies]
regex = "fancy-components/regex@1.0.0"
transcoder = "fancy-components/transcoder:1.0.0"
```
An example `component.wit`:

```wit
default world my-component {
  include wasi.command # a theoretical syntax for including a world in another
  export parse: func(x: string) -> result<_, string>
}
```

The resulting type for this component will be the same as the previous example, 
but it will also export a `parse` function of type `(string) -> result<_, string>`
as specified in the world.

The WIT document package name of `wasi` maps directly to the name of the 
dependency in `[package.metadata.component.target.dependencies]`.

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

[package.metadata.component.target]
path = "component.wit"
```

An example `component.wit`:

```wit
default world my-component {
  export greet: func(name: string) -> string
}
```

Here this component has no component registry dependencies and only exports a 
single function named `greet` with type `(string) -> string`.
