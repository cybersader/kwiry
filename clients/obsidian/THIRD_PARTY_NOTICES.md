# Third-party notices

Kwiry Search bundles portable Rust code and the official SQLite WebAssembly runtime inside its generated `main.js`.

## Kwiry portable core and Rust WebAssembly support

- `kwiry-core` is available under the MIT License or Apache License 2.0.
- `wasm-bindgen` and the Rust dependencies linked into the portable adapter retain their respective upstream license terms.
- Source and dependency identities are preserved by `daemon/Cargo.lock`, `rust/kwiry-obsidian-wasm/Cargo.lock`, `d5c-rust-license-inventory.json`, and `docx-rust-license-inventory.json` in the Kwiry source repository.

The Apache License 2.0 text is included at [`licenses/Apache-2.0.txt`](licenses/Apache-2.0.txt). Exact Rust dependency selections and required MIT, Unlicense, WHATWG BSD-3-Clause, and Unicode License v3 notices are included at [`licenses/Rust-DEPENDENCY-LICENSES.md`](licenses/Rust-DEPENDENCY-LICENSES.md).

## HTML named-character-reference data

The bounded HTML extractor includes a generated trie derived from the fixed `markup5ever` 0.14.1 named-entity table from the html5ever project. This is fixed derived source data, not a Cargo package or runtime dependency. The selected upstream MIT terms are preserved at [`licenses/markup5ever-entities-MIT.txt`](licenses/markup5ever-entities-MIT.txt), and exact source/generated/notice checksums plus table and notice counts are pinned under the explicit `fixed-derived-data` category in [`licenses/html-entity-provenance.json`](licenses/html-entity-provenance.json).

## Official SQLite WASM package

This plugin bundles `@sqlite.org/sqlite-wasm` version `3.53.0-build1`.

- Package wrapper: Apache License 2.0.
- SQLite core: public-domain dedication as distributed by the SQLite project.
- Emscripten-generated glue code: MIT License and University of Illinois/NCSA Open Source License. The complete upstream terms are included at [`licenses/Emscripten-LICENSE.txt`](licenses/Emscripten-LICENSE.txt).
- Official `sqlite3.wasm` byte length: `864752`.
- Official `sqlite3.wasm` SHA-256: `02d7e48164395fa68f81c6ec33e9da5461be397dc57602ac0cd89b4bbba1d312`.

Package source and notices are available from <https://github.com/sqlite/sqlite-wasm>, <https://sqlite.org/wasm/>, and <https://sqlite.org/copyright.html>.

## Omnisearch UX provenance

The search modal UX is informed by Omnisearch by Simon Cambier and contributors, licensed GPL-3.0. Adapted source files carry provenance comments.
