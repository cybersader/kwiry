# `kwiry checkroom` — room label preflight

`kwiry checkroom` answers, before a room is published, whether a governed room
directory would receive its publication label (`snapshot_id`), and if not,
exactly which files prevent it and why.

It runs the daemon's own labelling pass —
[`prepare_room`](../daemon/crates/kwiry-core/src/room_check.rs) (line 32), the
function the governed build calls from
[`build_partition`](../daemon/crates/kwiry-core/src/index.rs) (line 424) — so
there is no second walker or parser to keep in sync. That pass walks the room
beneath its captured root with the room-boundary rules of
[`room_walk.rs`](../daemon/crates/kwiry-core/src/room_walk.rs), reads every
admitted source through the same bounded, identity-checked reader, prepares it
with the same extractor and chunker, and applies the same per-source predicate
([`clean_snapshot_input`](../daemon/crates/kwiry-core/src/model.rs), line 900).

What is shared, and what is not: the walker, the reader, the parser and
chunker, and the per-file test are the daemon's own code, and the governed
build's room-level decision is made by `prepare_room` itself. The live daemon's
room-level labelling decision during reconcile is made separately in
[`runtime.rs`](../daemon/crates/kwiry-core/src/runtime.rs) (`observe_vault`,
around lines 1159–1290, and the label test around lines 2042–2048), which adds
metadata reuse on top of the same walk and per-file test. That path is not
shared code; it is covered by the agreement test's relabel pass, which forces a
strict-hash walk through it on every fixture room.

It builds no index, needs no data directory, takes no lock, uses no network and
writes nothing. Linux only, as governed room ingestion is.

## Usage

```bash
kwiry checkroom /absolute/path/to/room            # human lines
kwiry checkroom /absolute/path/to/room --json     # one JSON object on stdout
kwiry --config /path/config.toml --data-dir /path/data \
      checkroom --vault <VAULT_ID> /absolute/path/to/new-room --json
```

Without `--vault`, the room is checked alone. With `--vault`, the config is
loaded read-only, the named registration is re-pointed at the room (as a
publisher would before repointing the config), and the room is also checked for
overlap with every other registered room, the data directory and the other
Kwiry state paths, as the daemon checks it. The config must currently load.

The room path should be the absolute, canonical path the config will name. A
relative path is made absolute. A spelling the daemon would refuse — a
trailing separator, `.`, `..` or a doubled separator — is a usage error (exit
2), not a verdict. A canonically spelled path through a symbolic link is
reported as `room_boundary` (exit 1), because the daemon refuses that root.

There are no format or labelling knobs to pass: the set of admitted formats is
fixed by the build, and certification always uses the `strict_hash` basis.

Exit codes:

| Code | Meaning |
|---|---|
| 0 | The room would be labelled. |
| 1 | The room would not be labelled; see `offenders` and `walk_defects`. |
| 2 | Usage or setup error: bad arguments, a non-canonical room spelling, the room is not a directory, the config is missing or does not load, the `--vault` ID is not registered, or not Linux. |

A panic prints a fixed line and exits with Rust's panic code (101).

Output never contains file content or labels. Paths are room-relative.

## JSON

```json
{
  "would_label": false,
  "files_checked": 3,
  "offenders": [
    {"path": "broken.md", "reason": "invalid_frontmatter", "class": "content",
     "detail": "frontmatter is not valid YAML"},
    {"path": "huge.md", "reason": "section_limit", "class": "content",
     "detail": "file has more sections than the per-source section limit",
     "usage": {"sections": 100001, "heading_path_bytes": 100001}}
  ],
  "walk_defects": [],
  "limits": {"max_file_bytes": 10485760, "max_sections": 100000,
             "max_heading_path_bytes": 10485760, "max_chunks": 100000},
  "engine": {"version": "0.1.0", "commit": "<40 hex or null>",
             "build": "x86_64-unknown-linux-gnu/release",
             "extraction_policy_fingerprint": "<64 hex>",
             "source_preparation_schema_version": 11}
}
```

- `would_label` — the room would receive its configured label.
- `files_checked` — sources the walk admitted and the pass then prepared; see
  [What `files_checked` counts](#what-files_checked-counts).
- `offenders` — named entries whose removal from the room clears the finding.
  Sorted by path.
- `walk_defects` — failures no single droppable file explains: the room root
  itself (`path` is `""`), a directory that could not be opened or listed, an
  entry that could not be opened or stat'ed during the walk (`path` is that
  entry), or a directory holding an entry whose name is not UTF-8 (`path` is
  that directory).
- `usage` — present on `section_limit`, `heading_path_limit` and
  `chunk_inventory_limit` findings; see below.
- `class` — on every finding; see below.
- `limits` — the build's limits.
- `engine` — which build decided; see [Engine identity](#engine-identity).

`would_label` is the daemon predicate itself, not derived from the findings.
The agreement test also asserts `would_label` is true exactly when both lists
are empty.

## What `files_checked` counts

`files_checked` is the number of entries the walk admitted as sources and the
pass then handed to source preparation
([`check_room`](../daemon/crates/kwiry-core/src/room_check.rs), the
`files_checked += 1` in the `prepare_room` callback). The walk
([`room_walk.rs`](../daemon/crates/kwiry-core/src/room_walk.rs), `RoomRoots::walk`)
decides each entry in this order, and the first rule that applies wins: name
starts with `.` → not a valid UTF-8 name → cannot be opened or stat'ed →
directory → not a regular file → more than one hard link → extension not an
extractable format → larger than `max_file_bytes` (HTML exempt) → admitted.

| Entry | Counted in `files_checked` | Listed in `offenders` | Listed in `walk_defects` |
|---|---|---|---|
| Single-link regular file with an extractable extension (`.md`, `.txt`, `.canvas`, `.base`, `.excalidraw`, `.docx`, `.pdf`, `.xlsx`, `.xlsm`, `.html`, `.htm`; any case), at most `max_file_bytes` | yes | only when it is not a clean `*.md` source: any per-file reason, including `non_markdown_format`, `invalid_path` and a transient `io_unavailable` or `changed_during_read` read failure during preparation | no |
| Oversize `.html` / `.htm` (HTML is exempt from the walk's size check) | yes | yes, `non_markdown_format` | no |
| Any other oversize extractable source (`.md`, `.txt`, …) | **no** | yes, `over_size_budget` | no |
| Regular file whose extension is not an extractable format, or that has no extension (images, `README`), any size | no | no | no |
| Name starting with `.` at any depth, and everything beneath a dot-named directory | no | no | no |
| Regular file with more than one hard link, whatever its extension | no | yes, `hard_link` (every link in the room) | no |
| Symbolic link, or entry on another filesystem | no | yes, `boundary_escape` | no |
| Socket, FIFO or device node | no | yes, `special_file` | no |
| Entry whose name is not valid UTF-8 (and, for a directory, its subtree) | no | no | yes, `non_utf8_name` at the containing directory |
| Entry that could not be opened or stat'ed during the walk | no | no | yes, `io_unavailable` at the entry |
| Contents of a directory that could not be opened or changed identity | no | no | yes, `io_unavailable` at the directory |
| Directory whose listing fails part-way | entries listed before the failure: as above; the rest: no | as above, for the entries listed | yes, `io_unavailable` at the directory |
| Everything, when the room root is refused or another registered room root is found inside the room during the walk | no (`files_checked` is 0) | no | yes, `room_boundary` (or `io_unavailable`) at `""` |

So for a room whose `walk_defects` is empty, every regular file outside
dot-named paths is exactly one of: counted; an `over_size_budget` offender; a
`hard_link` offender; or ignored for its extension. For a copy that holds only
extractable sources, the file count equals `files_checked` plus the
`over_size_budget` and `hard_link` offenders.

Two edges, reported rather than changed: an oversize `.html` is counted and
reported as `non_markdown_format` while every other oversize source is not
counted and is reported as `over_size_budget` (so an oversize `.txt` is a
`content` finding while a small non-UTF-8 `.txt` is `non_markdown_format`); and
if another registered room root appears inside the room *during preparation*
(a directory replaced after the walk), `offenders` is cleared but
`files_checked` keeps the sources prepared before it was seen.
`files_checked_counts_exactly_the_sources_the_pass_prepared` in `room_check.rs`
pins every row except the walk-time open or `stat` failure, which needs a
failing system call, and the refused-root row (the transient preparation read
and the part-way listing failure are pinned through test hooks).

## Memory

`checkroom` holds a small record for every admitted source until the pass ends:
the walk's sorted file list (`enumeration.files`, one `DiscoveredFile` with its
absolute path, relative path and extension) and the walk's identity map used to
re-check each file when it is opened (`RoomRead::files`, relative path → device
and inode). Each source's prepared content is dropped as soon as it has been
classified. Measured on synthetic one-line notes in 1,000-note directories,
peak memory grows by about 500 bytes per source (52 MB at 50,000 notes,
126 MB at 200,000); longer paths cost more. Plan for roughly 0.5–0.8 GB per
million sources.

## Room roots and dot names

A room root whose own name starts with `.` (for example a staging copy at
`/srv/rooms/.staging-abc`) is supported: the dot-name rule applies only to
entries listed inside the room, never to the root or its ancestors. The
`checkroom` spelling check, the daemon's canonical-root check
([`room_boundary.rs`](../daemon/crates/kwiry-core/src/room_boundary.rs),
`canonical_room_root`), its OpenClast config validation and its ingestion probe
all accept it; `a_dot_named_room_root_is_walked_and_its_dot_entries_skipped`
pins this. Dot-named entries inside the room are skipped at every depth.

## Engine identity

`checkroom --json` and `selfcheck --json` print the same `engine` object from
one function (`daemon/crates/kwiry/src/engine.rs`), so a deployment can
record the checker's identity with a seal and prove that the serving binary is
the same build:

- `version` — the `kwiry` crate version.
- `commit` — the full 40-hex commit the binary was built from: taken from the
  `KWIRY_GIT_COMMIT` or `GIT_COMMIT` build environment variable, else from
  `git rev-parse HEAD` at build time; `null` when neither is available. An
  empty variable counts as unset. A build never fails for lack of git. A CI
  build of a pull request records the commit CI checked out, typically the
  merge commit rather than the branch head.
- `build` — `<target triple>/<cargo profile>`.
- `extraction_policy_fingerprint` — the fingerprint the daemon uses to bind
  prepared sources to this build's extractors.
- `source_preparation_schema_version` — the preparation schema of this build.

The commit records the checked-out `HEAD`; uncommitted changes in the build
tree are not reflected.

## Reasons and classes

Every cause that withholds the label is reported: each source the daemon would
not count as clean gets exactly one offender, and every walk failure gets a
finding. The reasons are a closed set; the serialized names are stable.

Each finding carries a `class`:

- `content` — decided by the file's bytes alone; the same bytes always give the
  same finding. A publisher may exclude the file and check again.
- `transient` — a read failed or raced. The result may differ next time; treat
  the copy as defective rather than excluding the file.
- `structural` — the room's shape withholds the label: a format, name, link,
  entry type or the root itself. Treat the copy as defective.

Reasons are matched on the fixed messages the walker, reader and extractors
emit. A unit test pins every matched message to the code that emits it, so a
renamed message fails the build's tests rather than changing class silently.
An unrecognised per-file message falls back to `io_unavailable` for a failed
read, `incomplete_extraction` for any other non-complete extraction, and
otherwise `unclassified`.

A file has one reason. Where several apply, structural reasons win over
content ones (a non-UTF-8 `.txt` file is `non_markdown_format`), so a
`content` finding is never hiding a structural cause.

| Reason | Class | List | Cause |
|---|---|---|---|
| `invalid_frontmatter` | content | offenders | Frontmatter opened and closed with `---` is not valid YAML. Unterminated frontmatter is body text and is fine. |
| `frontmatter_over_size` | content | offenders | Frontmatter is larger than its byte budget (the per-file budget) and is ignored. Unreachable while that budget equals `max_file_bytes`; listed so a future budget change cannot withhold silently. |
| `non_utf8_content` | content | offenders | Content is not valid UTF-8. |
| `nul_byte` | content | offenders | Content contains a NUL byte. |
| `over_size_budget` | content | offenders | File is larger than `max_file_bytes` (a file of exactly that size is fine). |
| `section_limit` | content | offenders | More than `max_sections` sections. |
| `heading_path_limit` | content | offenders | Heading-path bytes exceed `max_heading_path_bytes`. |
| `chunk_inventory_limit` | content | offenders | Prepared chunks exceed `max_chunks` although sections do not (long sections split into several chunks). |
| `incomplete_extraction` | content | offenders | The bytes extract to anything other than indexed-complete (partial, quarantined, unreadable, no text) for a cause not named above. |
| `non_markdown_format` | structural | offenders | An admitted non-Markdown source (`.txt`, `.canvas`, `.base`, `.excalidraw`, `.docx`, `.pdf`, `.xlsx`, `.html`, …) or Markdown not named `*.md` (`.markdown`, `.mdx`). Only `*.md` (any case) may stand under a label. |
| `invalid_path` | structural | offenders | The relative path is not a normalized forward-slash path, for example a name containing a backslash. |
| `hard_link` | structural | offenders | A regular file with more than one link. Every link inside the room is reported. |
| `special_file` | structural | offenders | A socket, FIFO or device node. |
| `boundary_escape` | structural | offenders | A symbolic link (to a file or a directory) or an entry on another filesystem. |
| `non_utf8_name` | structural | walk_defects | The directory at `path` holds an entry whose name is not UTF-8. The name itself cannot be printed. |
| `io_unavailable` | transient | either | An entry or directory — the room root included — could not be opened, listed or read, or a directory changed identity during the walk. |
| `changed_during_read` | transient | offenders | The file changed while it was read. Check again on a quiescent copy. |
| `room_boundary` | structural | walk_defects | The room root itself is refused: missing or unopenable, not canonical (for example a symbolic link), shared with or overlapping another registered room, overlapping Kwiry state, not on its expected mount, or another registered room root found inside it. |
| `unclassified` | structural | either | Withheld for a reason this build does not classify. Treat as not labelled. |

Names that start with `.` inside the room are skipped by the walk at every
depth and never affect the label; the room root's own name is not subject to
this rule (see [Room roots and dot names](#room-roots-and-dot-names)).

## Sections and heading-path bytes

These are counted on the Markdown body after frontmatter is removed, by the
same parse the extractor runs (`pulldown-cmark` with all extensions).

- **Section** — one per heading, plus one for the text before the first heading
  when the body does not start with a heading (an empty body counts as one).
  [`markdown_sections_with_budget`](../daemon/crates/kwiry-core/src/formats/markdown.rs)
  charges a section at line 93 for each heading and at line 104–105 for the
  leading text; the limit is enforced by
  [`ExtractionBudget::reserve_section`](../daemon/crates/kwiry-core/src/extract.rs)
  (line 38). More than `max_sections` refuses the file.
- **Heading path** — for each heading, the trimmed text of that heading and of
  its enclosing headings (each heading of a higher level still open above it).
  **Heading-path bytes** is the sum, over all sections, of the UTF-8 byte
  lengths of every heading in the section's path. A leading untitled section
  contributes 0. Example: `# A` then `## BB` then `## C` charges 1 + (1+2) +
  (1+1) = 6 bytes. More than `max_heading_path_bytes` refuses the file.
- **Prepared chunk** — each section's text split at `MAX_CHUNK_CHARS` (4,000
  characters) with overlap, empty parts dropped
  ([`source.rs`](../daemon/crates/kwiry-core/src/source.rs), line 345 for the
  chunk limit). Chunk preparation also charges, per chunk, its heading-path
  bytes plus the bytes of the space-joined heading text and of its normalized
  form (line 348–352) against the same `max_heading_path_bytes`; exceeding
  that is also reported as `heading_path_limit`.

The `usage` object reports `sections` and `heading_path_bytes` as defined
above, computed by the extractor's own accounting without stopping at the
limit. Extraction and chunk preparation refuse with the same message for the
section and chunk limits; `checkroom` reports `section_limit` when `sections`
exceeds `max_sections`, otherwise `chunk_inventory_limit`.

## Agreement test

`checkroom_matches_the_daemon_label_on_every_fixture_room`
([`checkroom_agreement.rs`](../daemon/crates/kwiry-core/src/runtime/tests/snapshots/checkroom_agreement.rs))
builds each fixture room with the governed daemon — a cold build plus boot
reconcile, then a label change that forces a `strict_hash` walk of the whole
room — and runs `check_room` on the same room. It asserts both daemon passes,
`checkroom` and a pinned expectation agree, and pins the reason classes. Cases:
a clean room; every case in `label_edges.rs`; and NUL bytes, `.txt`,
`.canvas`, `.markdown`, upper-case `.MD`, ignored non-source files, dot-named entries,
nested directories, a backslash in a name, hard links, file and directory
symbolic links, a socket, a non-UTF-8 name, and the section, heading-path and
chunk limits. `checkroom_matches_the_daemon_when_a_read_fails` adds a
deterministic transient case: a test hook fails every open of one source, in
the daemon and in the preflight alike, and both withhold the label
(`io_unavailable`, class `transient`).

## What could make them disagree

- **A changed room.** `checkroom` observes the room once. Check a sealed,
  quiescent copy and publish that same copy.
- **A different binary.** Limits, admitted formats and the parser belong to the
  build. Run the `kwiry` binary the daemon runs.
- **Different registrations or state paths.** Without `--vault`, overlap with
  other rooms and Kwiry state is not checked.
- **The daemon's index checks.** After a clean pass, the daemon also verifies
  that its new partition matches its manifest. A failure there is an index
  fault, not a property of the room.
- **The steady-state reconcile.** When neither the label nor the room changed,
  the daemon may keep a label using its metadata reuse rules; `checkroom`
  always reads everything, as the daemon does whenever it certifies a label.
