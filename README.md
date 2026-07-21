<p align="center">
  <img src="docs/logo/kwiry-graphite.svg" width="120" alt="Kwiry rounded-pixel search logo with graphite K">
</p>

<h1 align="center">kwiry</h1>

<p align="center">
  <strong>K</strong>nowledge <strong>W</strong>orkspace <strong>I</strong>nformation <strong>R</strong>etrieval <strong>Y</strong>oke — pronounced <em>“query”</em><br>
  <em>one daemon yoking all your knowledge trees into a single search</em>
</p>

<p align="center">
  <a href="https://github.com/cybersader/kwiry/actions/workflows/ci.yml"><img src="https://github.com/cybersader/kwiry/actions/workflows/ci.yml/badge.svg" alt="CI status"></a>
  <img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="MIT or Apache-2.0 license">
  <img src="https://img.shields.io/badge/Obsidian%20plugin-GPL--3.0--only-7c3aed.svg" alt="Obsidian plugin: GPL-3.0-only">
  <img src="https://img.shields.io/badge/Rust-000000.svg?logo=rust&logoColor=white" alt="Rust">
</p>

<p align="center">
  <strong>Local-first search for the knowledge you already own.</strong><br>
  kwiry combines Tantivy BM25, fully offline ONNX embeddings, and RRF hybrid ranking in one Rust binary — nothing leaves your machine, and no cloud API is required. Point it at any Markdown or text tree, not just an Obsidian vault, then search through its watching daemon, authenticated HTTP API, or BRAT-installable Obsidian client.
</p>

---

A single Rust binary provides:

- **Lexical search** (Tantivy BM25) over heading-split section chunks
- **Semantic search** (local ONNX embeddings, bge-small-en-v1.5, fully offline) — opt-in via `serve --semantic`
- **Hybrid ranking** (reciprocal rank fusion over both legs)
- A **watching daemon** with authenticated HTTP API, incremental hash-based updates, rename/delete correctness, and boot reconciliation for offline changes
- A **disposable index**: files are the sole source of truth; all derived state rebuilds from nothing, deterministically

## Quick start

Native installers are not published yet. Build the binary from source, then use the guided setup on native Windows or Linux:

```bash
cd daemon
cargo build --release -p kwiry
./target/release/kwiry setup
```

On Windows, run `target\release\kwiry.exe setup`. The wizard asks for a tree, a stable ID, whether to enable semantic search, and final confirmation. It prepares the index, installs a least-privilege per-user background service, starts it, and verifies authenticated readiness. WSL lifecycle setup is intentionally rejected; manual development commands remain available there.

Preview an automation-safe plan without changing anything:

```bash
./target/release/kwiry setup /absolute/path/to/notes --id notes --no-semantic --dry-run --json
```

See [the setup guide](docs/setup.md) for Windows Task Scheduler behavior, Linux `systemd --user`, semantic first-run costs, JSON automation, service lifecycle commands, readiness checks, and recovery.

For development or unsupported lifecycle environments, the original explicit flow remains available:

```bash
cargo run -p kwiry -- vault add --id notes --path /absolute/path/to/notes
cargo run -p kwiry -- index
cargo run -p kwiry -- search "your query"
cargo run -p kwiry -- serve --semantic
```

## Documentation

- [`docs/setup.md`](docs/setup.md) — guided setup, automation, per-user service lifecycle, and recovery
- [`CONTRACT.md`](CONTRACT.md) — the frozen product contract: invariants, HTTP/MCP surface, host profiles
- [`docs/vertical-1.md`](docs/vertical-1.md) — index + query core
- [`docs/vertical-2.md`](docs/vertical-2.md) — daemon, watcher, auth, incremental correctness
- [`docs/vertical-3.md`](docs/vertical-3.md) — semantic + hybrid search

Serve the repository documentation and logo previews without GitHub:

```bash
./scripts/serve-docs.sh local      # localhost only
./scripts/serve-docs.sh tailnet    # direct tailnet HTTP on port 32190
./scripts/serve-docs.sh tailscale  # tailnet HTTPS mounted at /kwiry
./scripts/serve-docs.sh unmount    # remove only the /kwiry HTTPS mount
```

The HTTPS mode preserves any existing Tailscale Serve root proxy and removes its `/kwiry` mount when the session exits.

## Repository layout

| Path | Contents | License |
|---|---|---|
| `daemon/` | Rust workspace: `kwiry-core` library + `kwiry` binary | [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE) |
| `clients/obsidian/` | Obsidian plugin (dumb client: query box + renderer + status light) | [GPL-3.0-only](clients/obsidian/LICENSE) |
| `fixtures/vault/` | CI fixture vault for determinism tests | MIT OR Apache-2.0 |
| `bench/` | Standalone benchmark crates (embedding runtime, vector store) | MIT OR Apache-2.0 |

Everything outside `clients/obsidian/` is dual-licensed under MIT or Apache-2.0 at your option (the Rust convention; Apache-2.0 adds an express patent grant). Unless you state otherwise, any contribution you submit to those paths is dual-licensed the same way, per Apache-2.0 §5.

The accepted responsive logo system and preserved design archive live in [`docs/logo/`](docs/logo/README.md): the untouched mark is used at 64px and below, while the graphite-K mark is used at 96px and above.

The Obsidian plugin is GPL-3.0-only and may port code from [Omnisearch](https://github.com/scambier/obsidian-omnisearch) by Simon Cambier. The plugin talks to the daemon only over localhost HTTP; the daemon and core contain no GPL code.

## Design invariants

1. Files are the sole source of truth — every byte of index state is derivable from the vault.
2. One contract, many hosts — desktop sidecar today; container sidecar and WASM lite tier share the same surface later.
3. Clients are dumb — no chunking, ranking, or index logic outside the daemon.
4. The engine is an implementation detail — Tantivy, sqlite-vec, and fastembed live behind adapters.
5. No algorithm authorship — scoring, ANN, tokenization, and embeddings are imported; RRF fusion is the one permitted formula.
