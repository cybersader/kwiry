# Seal-admission cases (shared with OpenClast)

`seal-admission-cases.v1.json` is a byte-for-byte copy of a case list kept by
the OpenClast project. Do not edit it here; take a new copy (and a new name if
the format changes) when the partner revises it.

- Repository: OpenClast (`openclast`)
- Branch: `task/search-sealer-20261123`
- Commit: `ecd07def1636ecf69dd42dee975e0d99a2bc1a80`
- Path: `sync-core/test/fixtures/seal-admission-cases.v1.json`

All content is synthetic test data: file names and bytes made up to exercise
frontmatter fences, YAML edge cases, encodings, size and section limits, and
file extensions. Nothing in it comes from a real vault.

Each case gives a room-relative `path`, its exact bytes (`base64`, or `prefix`
+ `repeat` × `times` + `suffix`), Kwiry's stated verdict at commit 657bced6
(`engine_rule_657bced6`, null where none was stated), the verdict measured with
a Kwiry binary built at 0ce9ff91 (`engine_0ce9ff91`), and OpenClast's own
admission decision with a reason code (`expected`, `reason`).

`tests/seal_admission_cases.rs` builds each case as a one-note room and asserts
that `check_room` (the `kwiry checkroom` pass) agrees with the stated column,
listing any disagreement explicitly.
