# sanitty-karet-lsp: karet-lsp 0.8.0, patched for sanitty

[karet-lsp](https://crates.io/crates/karet-lsp) 0.8.0 by Justin Chung (MIT OR
Apache-2.0, https://github.com/getkono/karet), with two fixes. It is published as
its own crate so sanitty can be installed from crates.io (`cargo install sanitty`,
`cargo binstall sanitty`); sanitty depends on it under the name `karet-lsp`
(`package = "sanitty-karet-lsp"`), and the library keeps the name `karet_lsp`. The
fixes:

1. `src/handshake.rs`: the hover client capability declares Markdown
   (`contentFormat`). Without it servers fall back to plain text, and TypeScript 7
   then glues the signature to the docs ("...: stringThe `pathname` ...") and sends
   its Markdown unrendered.
2. `src/pull.rs`: `textDocument/diagnostic` leaves an absent `identifier` out
   instead of sending `null`, which TypeScript 7 (Go) rejects as invalid params,
   so its problems never arrived.

Once a karet-lsp release contains both fixes, sanitty goes back to it and this crate
is no longer updated.

## License

karet-lsp is published under MIT OR Apache-2.0, at the user's option. This crate uses
and redistributes it under the MIT license; `LICENSE-MIT` is copied from the karet
repository, since the published crate ships without its license files. The two changes
above, and the package name and description, are the only modifications to the
published source.
