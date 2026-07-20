# Vertical 1: lexical index and query core

Vertical 1 registers one or more Markdown/text trees, rebuilds a disposable Tantivy index, and searches it through the `kwiry` binary. It deliberately has no watcher or daemon process yet.

## Ingestion policy (`chunking_version = 1`)

- Extensions: `.md`, `.markdown`, `.mdx`, `.txt`.
- Standard ignore files and hidden/VCS directories are respected; symlinks are not followed.
- Inputs must be UTF-8 and at most 10 MiB.
- Markdown is sectioned at headings. Plain text is one logical section.
- Oversized sections are split at paragraph/whitespace boundaries at 4,000 Unicode scalar characters with 400-character overlap.
- Frontmatter defaults: `title`, `description`, `tags`, `status`, `date`.
- Chunk IDs are SHA-256 over a versioned, unambiguous encoding of vault ID, normalized path, heading path, and global document chunk ordinal. Moving a file changes its IDs.


## Real-tree checkpoint

From `daemon/`:

```bash
cargo run -p kwiry -- --config /tmp/kwiry-config.toml --data-dir /tmp/kwiry-data vault add --id my-notes --path /absolute/path/to/tree
cargo run -p kwiry -- --config /tmp/kwiry-config.toml --data-dir /tmp/kwiry-data index
cargo run -p kwiry -- --config /tmp/kwiry-config.toml --data-dir /tmp/kwiry-data search "your vocabulary"
```

Use `--json` on `search` for stable machine-readable output. Delete `/tmp/kwiry-data` at any time; `index` reconstructs it from registered files.
