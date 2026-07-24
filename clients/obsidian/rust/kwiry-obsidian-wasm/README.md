# Kwiry Obsidian production Rust WASM adapter

This standalone crate is the browser-Worker boundary between the production Obsidian plugin and `kwiry-core` with only the `portable` feature enabled. It exposes three bounded, versioned operations:

- prepare source bytes from a strict `SourceDescriptor` envelope;
- prepare lexical query data and an optional fixed metadata-probe plan;
- rerun/finalize query preparation from the original query plus the Boolean probe result and return an allowlisted opaque FTS5 MATCH plan.

The adapter exposes no filesystem, Obsidian, SQLite, SQL statement, network, persistence, or Worker capability. TypeScript selects fixed SQL constants by `plan_id` and binds the returned `match_value`; it does not tokenize, quote, translate fields, or mutate query kinds. Unsupported explicit syntax returns `explicit_query_unsupported` without echoing query text.

`wasm-bindgen` is pinned to 0.2.126. Generated `target/` and `pkg/` output is disposable and must not be committed.

## Reproduce

Prerequisites are Rust 1.95.0, the `wasm32-unknown-unknown` target, `wasm-bindgen-cli` 0.2.126, and Node 22 or 24.

```bash
cargo fmt --manifest-path clients/obsidian/rust/kwiry-obsidian-wasm/Cargo.toml --all -- --check
cargo clippy --manifest-path clients/obsidian/rust/kwiry-obsidian-wasm/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path clients/obsidian/rust/kwiry-obsidian-wasm/Cargo.toml
cargo check --manifest-path clients/obsidian/rust/kwiry-obsidian-wasm/Cargo.toml --target wasm32-unknown-unknown
npm test --prefix clients/obsidian/rust/kwiry-obsidian-wasm
```

The final command compares the same strict fixture set through a native executable and the Node-loaded production WASM bindings. This is adapter evidence only; the production SQLite Worker and active-vault lifecycle are separate Gate 4 and Gate 5 boundaries.
