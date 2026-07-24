# Third-party notices

Kwiry Search bundles portable Rust code and the official SQLite WebAssembly runtime inside its generated `main.js`.

## Kwiry portable core and Rust WebAssembly support

- `kwiry-core` is available under the MIT License or Apache License 2.0.
- `wasm-bindgen` and the Rust dependencies linked into the portable adapter retain their respective upstream MIT, Apache-2.0, or MIT-or-Apache licensing.
- Source and dependency identities are preserved by `daemon/Cargo.lock` and `rust/kwiry-obsidian-wasm/Cargo.lock` in the Kwiry source repository.

The Apache License 2.0 text is included at [`licenses/Apache-2.0.txt`](licenses/Apache-2.0.txt). The MIT License text is available from <https://opensource.org/license/mit>.

## Official SQLite WASM package

This plugin bundles `@sqlite.org/sqlite-wasm` version `3.53.0-build1`.

- Package wrapper: Apache License 2.0.
- SQLite core: public-domain dedication as distributed by the SQLite project.
- Official `sqlite3.wasm` byte length: `864752`.
- Official `sqlite3.wasm` SHA-256: `02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312`.

Package source and notices are available from <https://github.com/sqlite/sqlite-wasm>, <https://sqlite.org/wasm/>, and <https://sqlite.org/copyright.html>.

## Omnisearch UX provenance

The search modal UX is informed by Omnisearch by Simon Cambier and contributors, licensed GPL-3.0. Adapted source files carry provenance comments.
