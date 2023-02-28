# Component example

This directory contains an example component implementing a simple "passthrough 
cache" service responsible for fetching the content bytes of a given URL from a 
supplied origin service.

The component imports two interfaces: a cache implementation for storing 
previously fetched content and an "origin" backend implementation for 
forwarding the request to when there is a cache miss.

It exports the same backend interface as it imports, effectively wrapping the 
provided import interface with some simplistic caching logic.

## Building the component

To build the component, run the following command:

```
cargo component build
```

The component should now exist at `target/wasm32-wasi/debug/service.wasm`.

The resulting component will have the following imports:

```wat
(import "cache" (instance (type ...)))
(import "origin" (instance (type ...)))
```

And export the following:

```wat
(export "backend" (instance ...))
```
