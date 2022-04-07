# Component example

This directory contains an example component implementing a simple "passthrough" service
responsible for fetching the content bytes of a given URL from a supplied origin service.

The component imports two interfaces: a cache implementation for storing previously fetched content
and a backend implementation for forwarding the request to when there is a cache miss.

It exports the same backend interface it is given as the origin.

## Building the component

To build the component, run the following command:

```
cargo component build
```

The component should now exist at `target/wasm32-unknown-unknown/debug/service.wasm`.

The resulting component will have the following imports:

```wat
(import "cache-0.1.0" (instance (type ...)))
(import "backend-0.1.0" (instance (type ...)))
```

And export the following:

```wat
(export "backend-0.1.0" (instance ...))
```
