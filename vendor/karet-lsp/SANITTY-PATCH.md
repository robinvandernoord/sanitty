# karet-lsp 0.8.0, patched for sanitty

The published karet-lsp 0.8.0 (MIT OR Apache-2.0, https://github.com/getkono/karet),
used through `[patch.crates-io]` in sanitty's `Cargo.toml` until upstream fixes these:

1. `src/handshake.rs`: the hover client capability declares Markdown
   (`contentFormat`). Without it servers fall back to plain text, and TypeScript 7
   then glues the signature to the docs ("...: stringThe `pathname` ...") and sends
   its Markdown unrendered.
2. `src/pull.rs`: `textDocument/diagnostic` leaves an absent `identifier` out
   instead of sending `null`, which TypeScript 7 (Go) rejects as invalid params,
   so its problems never arrived.

Drop this directory and the `[patch.crates-io]` entry once a karet-lsp release
contains both fixes.

## License

karet-lsp is published under MIT OR Apache-2.0, at the user's option. sanitty uses and
redistributes it under the MIT license; `LICENSE-MIT` is copied from the karet
repository, since the published crate ships without its license files. The two changes
above are the only modifications to the published source.
