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

  * The design should enable users to use on components from a registry without 
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
    
  * `world` - a path to a wit document specifying a world definition for the 
    component being authored; it must contain a `world` with the same name as 
    the current crate name.

  The `targets` and `world` fields are mutually exclusive.

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
  registries), the short-form differs from what is supported by `cargo`:

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

  Dependencies may specify a specific registry to use by specifying the `registry` field:

  ```toml
  foo = { package = "ns/foo", version = "0.1.0", registry = "my-registry" }
  ```

  The `default` name is used when no `registry` field is specified in a
  dependency. Therefore, specifying a registry with name `default` will 
  override the built-in default in `cargo-component` (expected to be a 
  future Bytecode Alliance component registry instance).

  A registry URL may use the `file://` scheme to point at local directory that 
  may have vendored packages. This "local registry" will be the first supported 
  registry implementation in `cargo-component` while the implementation of 
  component registries is still in progress.

  It should be possible to specify the registries at the workspace root `Cargo.toml` as well, allowing for a single set of registries to be used across a workspace.

#### The `targets` field

The `targets` field in `[package.metadata.component]` is used to specify a 
world the component is intended to target.

If not specified, the component will only import from component package 
dependencies by default.

The syntax of the value is identical to referencing a world from a package in a 
`wit` document:

```
"<package-name>.<world-name>"
```

Here `package-name` is the name of one of the dependencies in `[package.metadata.component.dependencies]`.

Assuming an entry for a `wasi` dependency in `[package.metadata.component.dependencies]`
and the package contains a world named `command`, a component being authored 
may specify a value of `wasi.command` to target the `command` world, for 
example.

Bindings will be automatically generated for the specified world's imports and 
exports.

It is an error to specify a name of a package that is not specified in the 
dependencies table.

It is also an error to specify a world name that is not defined in the 
specified package.

#### The `world` field

The `world` field in `[package.metadata.component]` is used to specify the path 
to a wit document defining the authored component's world; the document must contain a world with the same name as the current crate.

The field exists to grant developers explicit control over the _component type_ 
(i.e. its imports and exports) of the component being authored.

The `world` field is mutually exclusive with the `targets` field as it 
is expected that wit should be able to express the targeting relationship (i.e. 
via a proposed `include` syntax, perhaps).

The presence of a `world` field alters the [behavior of dependencies](#dependency-behavior).

#### Dependency behavior

The presence of a `world` field in `[package.metadata.component]` alters the 
behavior of dependencies.

When the `world` field _is not_ specified:

* There should be at most one wit package dependency and it must 
  be used in the `targets` field, if present; defining a dependency on a wit 
  package not used in a `targets` field will result in an "unused dependency" 
  warning from `cargo-component`.

* Any number of component package dependencies are allowed; bindings will be 
  generated to (instance) import the component's directly exported functions; 
  the imports are _in addition to_ any imports from a targeted world.

When the `world` field _is_ specified:

* Any number of wit and component package dependencies are allowed.

* The dependencies are mapped to package references in the specified wit 
  document.

* Component package dependencies are not automatically translated to imports as 
  it is expected the the world being defined will describe how to import them.

## Tooling to extract a world from `Cargo.toml`.

It is expected that `cargo-component` will have a subcommand for extracting a 
wit document from a `Cargo.toml` file that does not already contain a `world` 
field.

It would then add the `world` field to the `Cargo.toml` to point at the 
extracted wit document.

The extracted wit document should be semantically equivalent to the world 
synthesized by `cargo-component` from the `targets` field and component package 
dependencies when the `world` field is not present.

## Examples

### Targeting only a world from a registry

An example `Cargo.toml`:

```toml
[package]
name = "my-component"
version = "1.2.3"

[dependencies]
# Rust crate dependencies here

[package.metadata.component]
package = "my-org/my-component"
targets = "wasi.command"

[package.metadata.component.dependencies]
wasi = "webassembly/wasi:1.2.3"
```

The above might be the future default output from `cargo component new`.

Here the component being authored _may_ import what is expected to be imported 
by the `command` world and _must_ export what is expected to be exported 
by the `command` world via the generated bindings.

In theory, the authored component could then simply run in any host that 
supports the `wasi.command` world (e.g. a future wasmtime CLI).

### Targeting a world and using other components from a registry

An example `Cargo.toml`:

```toml
[package]
name = "my-component"
version = "1.2.3"

[dependencies]
# Rust crate dependencies here

[package.metadata.component]
package = "my-org/my-component"
targets = "wasi.command"

[package.metadata.component.dependencies]
wasi = "webassembly/wasi:1.2.3"
regex = "fancy-components/regex:1.0.0"
transcoder = "fancy-components/transcoder:1.0.0"
```

In this example, the component still targets the `wasi.command` world as above.

However, it will also import an _instance_ named `regex` and an instance named 
`transcoder` that export the functions directly exported by the `regex` and 
`transcoder` components, respectively.

The component produced by `cargo-component` will contain URL references to the 
component dependencies and these serve as hints for later composition tooling 
to instantiate those particular components and link them with this one by 
default. As they are instance imports, the authored component may still be 
linked against alternative implementations provided they implement the expected 
interfaces according to component model subtyping rules.

Running the proposed command to generate a wit document from this `Cargo.toml` should create a wit file that looks something like:

```wit
world my-component {
  include wasi.command # proposed syntax for including a world in another

  import regex: regex
  
  import transcoder: transcoder
}
```

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
world = "./component.wit"

[package.metadata.component.dependencies]
wasi = "webassembly/wasi:1.2.3"
regex = "fancy-components/regex:1.0.0"
transcoder = "fancy-components/transcoder:1.0.0"
```

An example `component.wit`:

```wit
world my-component {
  include wasi.command # proposed syntax for including a world in another

  import regex: regex
  
  import transcoder: transcoder
  
  parse: func(x: string) -> result<_, string>
}
```

Because the `world` field is specified, the dependencies are interpreted as wit 
document packages only; `regex` and `transcoder` are not imported automatically 
as in the previous example.

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
world = "./component.wit"
```

An example `component.wit`:

```wit
world my-component {
  greet: func(name: string) -> string
}
```

Here this component has no component registry dependencies and only exports a 
single function named `greet` with type `(string) -> string`.

### Exporting an interface directly from a component

An example `Cargo.toml`:

```toml
[package]
name = "my-component"
version = "1.2.3"

[dependencies]
# Rust crate dependencies here

[package.metadata.component]
package = "my-org/my-component"
world = "./component.wit"

[package.metadata.component.dependencies]
http-types = "http/types:1.2.3"
```

An example `component.wit`:

```wit
world my-component {
  import downstream: http-types.handler
  
  include http-types.handler # proposed wit syntax
}
```

In this example, the authored component doesn't target any particular world.

It imports a purely abstract HTTP `handler` interface from wit package `http/types`
with name `downstream`. As this import comes from a wit package and not a 
component package, it offers no hints to any composition tooling as to what 
implementation of the downstream "handler" to link with in the future.

It then exports the same handler interface directly from the component by including its exports.

This allows the component to act as a middleware for some other HTTP handler: 
it may forward requests to the downstream handler (possibly post-processing the 
response) or it may respond to the request itself.

### Exporting a named interface from a component

An example `Cargo.toml`:

```toml
[package]
name = "my-component"
version = "1.2.3"

[dependencies]
# Rust crate dependencies here

[package.metadata.component]
package = "my-org/my-component"
world = "./component.wit"

[package.metadata.component.dependencies]
http-types = "http/types:1.2.3"
```

An example `component.wit`:

```wit
world my-component {
  import downstream: http-types.handler

  handler: http-types.handler
}
```

This example is nearly identical to the previous, except instead of directly 
exporting the handler functions from the component, it exports an instance 
named `handler` that exports the functions.

The difference lies in the wit syntax: the former example uses the `include` 
syntax to indicate the interface's exports should be included in the world's 
exports and this example exports the interface with the name `handler`.
