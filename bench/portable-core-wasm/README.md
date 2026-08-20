# Kwiry portable-core WebAssembly parity witness

This standalone probe verifies Gate 3's portable Rust boundary without adding `wasm-bindgen` to `kwiry-core` or changing the daemon workspace. It depends on `kwiry-core` with default features disabled and only `portable` enabled.

The witness executes the same canonical fixture set twice:

1. through a native Rust binary;
2. through a `wasm32-unknown-unknown` build loaded by Node using `wasm-bindgen` 0.2.126.

The compact JSON outputs must be byte-for-byte identical. The Node harness also checks source validation, raw-byte hashing, UTF-8 and NUL skips, Markdown/frontmatter/wikilinks, CRLF handling, heading chunking and overlap, technical identifiers, bounded HTML title metadata, primary/latent separation, malformed recovery and whole-source quarantine, path-derived rename identity, string-encoded nanoseconds, portable API/status serialization, ordinary/explicit/identifier query plans with backend-neutral Boolean intent, fixed lowercase metadata probes, and inert SQL-looking input.

Generated `target/` and `pkg/` artifacts are disposable and must not be committed.

## Reproduce

Prerequisites:

- Rust 1.95.0;
- `wasm32-unknown-unknown` target;
- `wasm-bindgen-cli` 0.2.126;
- Node 24.

The final validator runs the complete native/WASM witness, including the pinned `wasm-bindgen` CLI step:

```bash
CARGO_BUILD_JOBS=1 npm test --prefix bench/portable-core-wasm
```

That command is intentionally reserved for the final validation lane. A bounded native fixture precheck that does not invoke `wasm-bindgen` is:

```bash
CARGO_BUILD_JOBS=1 cargo run --quiet --manifest-path bench/portable-core-wasm/Cargo.toml --locked --bin kwiry-portable-core-native -- bench/portable-core-wasm/fixtures/cases.json
```

The final line is compact evidence similar to:

```json
{"status":"pass","cases":27,"native_bytes":12345,"wasm_bytes":12345,"byte_identical":true}
```

This proves deterministic portable preparation and query-planning parity only. It does not deliver the Obsidian in-plugin backend, FTS5 adapter, active-vault indexing, or production Worker packaging.
