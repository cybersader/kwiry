# Contributing to Kwiry

Files are the source of truth, indexes are disposable, and `CONTRACT.md` is frozen except for owner-approved amendments. Read [`CONTRACT.md`](CONTRACT.md) and [`ROADMAP.md`](ROADMAP.md) before proposing behavior changes.

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | from [`rust-toolchain.toml`](rust-toolchain.toml) | `rustup` installs the pinned channel with `clippy` and `rustfmt` automatically |
| Node.js | 24 (CI also tests 22) | needed only for the Obsidian client and WASM parity checks |
| wasm-bindgen-cli | 0.2.126 | `cargo install wasm-bindgen-cli --version 0.2.126 --locked` |

## Local checks that mirror CI

Run everything CI's main jobs run:

```bash
make check
```

Or run each component on its own.

**Daemon** (`make check COMPONENT=daemon`):

```bash
cd daemon
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace
```

**Obsidian client** (`clients/obsidian`):

```bash
cd clients/obsidian
npm ci
npm test
npm run corpus:smoke
npm run build
```

`make check COMPONENT=obsidian` runs the client's lint, typecheck, tests, and version checks. CI additionally runs `npm run test-vault:smoke`, evidence generation, and the privacy validation; see [`.github/workflows/ci.yml`](.github/workflows/ci.yml) for the full list.

## Pull requests

- Keep each PR to one concern, with tests for any behavior change.
- Use synthetic fixtures only. Never commit real notes, diagnostics, tokens, or machine-specific absolute paths (CI rejects home-directory paths in first-party source).
- Machine-specific settings belong in untracked local files such as `CLAUDE.local.md`, never in tracked files.
- No AI attribution or generated-by trailers in commits.
- Report security issues privately as described in [`SECURITY.md`](SECURITY.md).
