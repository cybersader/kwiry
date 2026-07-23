# Tantivy 0.26.1 WebAssembly feasibility probe

## Result: normal incremental writer NO-GO

This standalone probe tests the first approved D5B engine path without changing the daemon workspace or attempting to make all of `kwiry-core` compile for WebAssembly.

On Rust 1.95.0, Node 24.12.0, `wasm32-unknown-unknown`, and `wasm-bindgen` 0.2.126:

- native `SingleSegmentIndexWriter` control: **pass**;
- native standard `IndexWriter` add/commit/delete/reload control: **pass**;
- minimal Tantivy 0.26.1 WASM compilation: **pass**;
- Node execution of RAM-backed `SingleSegmentIndexWriter`, manual reader, standard tokenizer, `QueryParser`, BM25, `TopDocs`, stored fields, and reopen: **pass**;
- Node construction of standard `IndexWriter`: **fail** with `Failed to spawn segment updater thread`.

The generated release WASM for the three-document probe was approximately 4.2 MB before `wasm-bindgen` processing and 3.9 MB afterward. These are probe sizes, not a production bundle measurement.

## Interpretation

Tantivy's thread-free fresh-index path can run in ordinary single-threaded WebAssembly. Tantivy 0.26.1's normal writer cannot: it creates explicit Rayon pools and native indexing workers before Kwiry can exercise commit, term deletion, incremental replacement, or merge shutdown.

The approved D5B path prohibits a Tantivy fork, custom writer/threading internals, duplicate TypeScript engine, or bespoke tokenizer/ranker. Therefore the normal incremental Tantivy-WASM route is a technical **NO-GO**.

`SingleSegmentIndexWriter` leaves one materially different possibility: rebuild a complete fresh in-memory index for every vault mutation. That route has not been approved and is not classified as GO. It changes update latency, CPU, memory, event coalescing, and atomic-generation behavior enough to require a separate owner decision and benchmark.

Failure of this probe does not authorize an automatic FTS5-WASM or JavaScript-engine pivot.

## Package shape

- `src/lib.rs` exports separate `probe_single_segment` and `probe_index_writer` functions.
- Native tests exercise both paths before a WASM failure is interpreted.
- `test/node-probe.cjs` calls one export at a time and preserves stage-qualified errors.
- Generated `target/` and `pkg/` artifacts remain ignored; the standalone lockfile pins the tested dependency graph.

## Reproduce

```bash
cargo test --manifest-path bench/tantivy-wasm/Cargo.toml --locked
rustup target add wasm32-unknown-unknown
cargo build \
  --manifest-path bench/tantivy-wasm/Cargo.toml \
  --target wasm32-unknown-unknown \
  --release \
  --locked
wasm-bindgen \
  --target nodejs \
  --out-dir bench/tantivy-wasm/pkg \
  --out-name kwiry_tantivy_wasm_probe \
  bench/tantivy-wasm/target/wasm32-unknown-unknown/release/kwiry_tantivy_wasm_probe.wasm
node bench/tantivy-wasm/test/node-probe.cjs single-segment
node bench/tantivy-wasm/test/node-probe.cjs index-writer
```

Expected final command failure:

```text
index_writer:create_writer: System error.'Failed to spawn segment updater thread'
```

Do not add WASM threads, patch Tantivy, or treat the passing single-segment result as incremental-host approval without a new owner decision.
