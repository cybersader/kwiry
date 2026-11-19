//! Executable, model-checked holder lifecycle for Kwiry as an OpenClast
//! `Holdings` consumer (IG-2A design §5, frozen contract HOLDINGS-WIRE
//! amendments A–H).
//!
//! The model is **normative**: the design document's §5.8 transition table
//! is rendered from [`rows`] by [`table::render`], and the test suite fails
//! if the two drift. The crate has no I/O and no dependencies.
//!
//! # Structure
//!
//! - [`state`]: the holder's durable store (holder directory and data root),
//!   its volatile process state, the authority, the network, and the
//!   remaining adversary budgets. The eleven machines are Rust types:
//!   M1 [`state::Phase`], M2 `Durable::{hw, accept}`, M3 `Volatile::fetching`
//!   and `Net::list`, M4a [`state::ApplyStep`], M4b [`state::Class`],
//!   M5 [`state::UMem`] and [`state::UDur`], M6 and M8 [`state::Pending`] with
//!   [`state::SlotVol`], M7 [`state::IDur`], M9 [`state::Rsv`], M10
//!   [`state::Route`].
//! - [`step::exec`]: the pure transition function. Every durable write is a
//!   separate step, so a crash can fall between any two; a crash keeps only
//!   durable state. Every step cites the [`row_id::RowId`]s it implements.
//! - [`action`]: the events the checker interleaves: holder actors (list
//!   apply, report and status senders, transport, GC and purge, release),
//!   the healthy authority and network, the human release decision, the
//!   operator, and budgeted faults, crashes and scenario changes.
//! - [`props`]: invariants I1–I9 and the liveness goal L1 (every retirement
//!   reaches `complete` or `blocked`, every epoch acceptance is answered by a
//!   status accepted in the new epoch, and minting resumes after an epoch
//!   change, through member reports or the lease timeout).
//! - [`checker`]: bounded explicit-state BFS with crash injection, plus a
//!   strong-fairness SCC analysis for L1.
//! - [`design`]: revision 8 as written ([`design::Design::REV8`]) and each
//!   repair as a separate [`design::Fix`]; [`design::Design::REV11_5`] has
//!   all.
//!
//! # Universe and bounds
//!
//! Two configured rooms (`R0`, `R1`), at most one obligation per room (so
//! two obligations), three units (`U0` and `U2`, each room's current object;
//! `U1`, R0's superseded object, present when a profile enables GC work),
//! two report channels and the status channel, two epochs (one supported
//! restore), two release instructions, and per-profile budgets for crashes
//! (≤ 2), faults (≤ 2, from up to eight families), list changes, room
//! retirements, retention drops and restores. A profile states its bounds;
//! `tests/model_check.rs` lists the profiles the test suite runs.
//!
//! Capacities inside a state (messages in flight per channel, uncertain keys,
//! authority receipts) are checked, never truncated: overflowing one fails
//! the run as a bound error.
//!
//! # Encoding revision 8
//!
//! Where the revision-8 table is silent or ambiguous, the model takes the
//! most charitable reading and says so:
//!
//! - R12, R13, R14 and R15 commit inside A1's W (§5.2 allows batching).
//! - Side files before a W (`sent/`, `receipts/`, `manifests/`) fold into
//!   that W: a crash between them only leaves an orphan.
//! - `pin_and_lease` reads the committed tombstones as well as the snapshot
//!   (§5.4 lists "not tombstoned" separately).
//! - R1 replacing a sent report moves its possibly-transmitted key to
//!   `uncertain`; A1 removing or invalidating a pending report cancels its
//!   queued transport attempt.
//! - O7 for a GC claim has no owner to keep, so the entry is unattributed;
//!   O5 does not re-check a GC reason at claim time.
//! - C9 (tombstoned but listed active) keeps the data refused, withheld and
//!   attributed to the listed holding.
//! - The transport's dispatch, absent from the table, sends whatever was
//!   handed off.
//!
//! # Reductions (sound for the checked properties)
//!
//! - The checker explores the quotient [`step::canonical`]. It prunes
//!   authority idempotency receipts for request bodies that can never be
//!   presented again, erases the parts of the holder's floor and accepted
//!   status bodies that only an equality with a newer body could read, and
//!   erases audit-only authority records (and the journal once no restore
//!   remains). It also keeps only the rank of an obligation receipt's commit
//!   seq (read only to find the newest), erases a zero-count, result-free
//!   status once no live or derivable body can fall below its `list_ref`
//!   (authority) or be re-derived equal to it (holder), and erases all but
//!   the state of an invalidated slot's key (never transmitted; R6, R13 and
//!   R18 read only its state). With static pins (outside the root-pin
//!   probe) it erases the holder's pin-history, which then cannot conflict.
//!   `tests/projection.rs` checks the bisimulation condition (same
//!   enabled actions, rows, observations, properties and projected
//!   successors) on reachable states of four profiles.
//! - A failed deletion that changes nothing (L5f, O11 and O11g failures) is a
//!   stutter: it spends no fault budget and its successor is its source. That
//!   admits any number of such failures, a superset of the budgeted ones, and
//!   adds no states. Failures that change state (O7) still spend budget.
//! - A relist is explored only when the list's items or instructions would
//!   change, the epoch changes, the holder must relist (`relist`), the
//!   authority is rollback-suspected, an admitted list is not yet applied, or
//!   a focused key/lease probe needs it. A relist that only advances the
//!   revision (a seq-only or counts-only transaction) is not explored. A2n
//!   and A3n run inside the preceding step.
//! - A human decline that blocks no modeled obligation is audit only (a seq
//!   bump) and is not explored.
//! - A purge pass scans after every revoked unit had a deletion attempt
//!   (§5.6 steps 4–6 then 7); failed deletions are quarantined and do not
//!   block the scan.
//! - Optional counts-only same-state reports (HW §14 Q7) are explored only
//!   in profiles that enable them; they are never needed for progress.
//! - In symmetric retirement/crash profiles, the first retirement names R0
//!   in the canonical state (R0 ↔ R1, U0 ↔ U2, report channels 0 ↔ 1, and
//!   directories D0 ↔ D1 with the two initial `pin_ref`s, which are static
//!   outside the root-pin probe). Before that event, both rooms have
//!   identical roles and no per-room work.
//!   After it, the first retirement fixes the labels for the entire suffix;
//!   in particular no healthy edge swaps labels, so the checker keeps the
//!   original per-actor fairness classes, not fairness across room orbits.
//!   Both branches have the same property verdicts and row coverage under
//!   renaming. Paths replay the concrete representative's first retirement.
//! - In the H.30 fixtures, the two other holders are interchangeable (every
//!   fact about one is in its [`state::Peer`] record, every peer action exists
//!   for each index, and no property or observation names a position), so
//!   the canonical state sorts them. `tests/rev11_2.rs` checks that the
//!   verdicts and rows match the unreduced graph. The quotient also erases
//!   the peers' seats while no gate is closed (nothing reads them) and their
//!   journaled roster entries when the journal is corrupt and H.14 fences it
//!   (no supported restore can succeed, and `restore_journal_lost` replays
//!   no journal), and it renames a peer's key generation to 0 while no
//!   backup or journaled copy exists and its signing key matches its roster
//!   entry (only that equality is read). The shared restore fixture leaves
//!   out a `403` the peer receives, by simulation: it only drops an
//!   earlier-epoch lease. The H.33 operator epoch and its list cursors are
//!   explored in a fixture of their own, without the restore. The shared
//!   restore fixture leaves cursors out; the holders keep them across a
//!   restore, and a restore-cursor fixture (`tests/rev11_4.rs`) presents
//!   them to the restored authority with one old-lineage `holder_rev`
//!   advance after the backup (H.41, H21). The H.44 raw restore has a
//!   fixture of its own: the raw-restored instance's suspected start (H20),
//!   the old instance still issuing lists, the confirmation by tooling or
//!   attestation (H19), and the operator's procedure or
//!   `restore_journal_lost`. Two attestation fixtures (`tests/rev11_5.rs`)
//!   add the H.48.1 precondition, with failed source checks that may age
//!   past 1 h (H22), and a false attestation after which the old instance
//!   keeps issuing lists and minting, the exit's key rotations (H23), and
//!   the other holders applying them, fetching a new-epoch list while they
//!   still serve the earlier one (H24).
//! - States are stored as 128-bit fingerprints (hash compaction).
//!
//! # Coverage limits
//!
//! The core keeps at most two rooms/obligations/instructions and one supported
//! restore. It treats tags, signatures, list pagination and request bodies as
//! verified symbols. Appendix A tests simulate bytes and receipt/commit time
//! with a deterministic fake, not HTTP or cryptography. Only focused fixtures
//! exercise absolute lease lapse, a holder swap, interrupted backup recovery,
//! a lost-state reset, one live handle, one late builder publication, the
//! H.12 validation order (an injected request envelope), the H.13 clock
//! boundaries, a corrupt retirement journal and the audited
//! `restore_journal_lost` override (one symbolic flag, not a hash chain),
//! the H.21 capability/clock rules at admission and release, the H.23, H.26
//! and H.34 clock hold (true lag, which may be negative, and round trip as
//! abstract seconds; the in-memory raise and its separate durable write; the
//! clear written first, its failed write and a crash after it; the probe and
//! its 60 s window), the H.25, H.30, H.32 and H.33 mint gate with two other
//! engine holders of the id-space, and the H.16 root pins (attestation,
//! replay and orphan reservation, the journal-lost reset, storage moves and
//! swaps, rebuilds and the stale bound; the core profiles carry static pins).
//! The other holders are abstracted to what the gate reads: a roster entry
//! (enrolled, enabled, key generation, and whether its refusal on every route
//! is older than the lease timeout) with its backup copy and journaled entry
//! (H.31), a seat, the epoch of the list each serves under a live lease, a
//! list cursor in flight, and whether it will ever report. Their own
//! lifecycle is this machine. A decommissioned holder is modeled as a
//! disabled one. A holder's earlier-epoch list is served until it applies a
//! current-epoch list, receives a `403`, or reaches the lease timeout,
//! whatever its enable state: disabling it stops nothing by itself (H.32.1),
//! and a `403` may never arrive. Lease policy changes are journaled but not
//! modeled: the timeout uses the deployment ceiling, which every enrolled
//! `serve_seconds` is below. Time is a probe clock in seconds from the
//! instant lists and minting stopped; time before it is abstracted to the
//! aged-refusal step, and the lease timeout is one fair step to `T_stop +
//! 1051 s`. In the gate fixtures a restore's confirmed stop (H19) and its
//! first transaction are separate steps, so the lease timeout can fall
//! between them (H.33.3). The old instance is abstracted to the lists it
//! issues the other holders after `T_stop` and, after a false attestation,
//! the capabilities it mints for them. A verification and a true attestation
//! stop it (none under H.44). A false attestation is explored in its own
//! fixture: with every H.48 and H.49 fix, the exposure that remains is the
//! named residual (a holder that has not applied the exit's key-set
//! statement, or an engine that has not cleared the gate), which the
//! fixture shows is reachable and records rather than counts as a
//! violation. The key rotations are abstracted to whether each other holder
//! applied them; the modeled holder's own key-set machinery (H.6, H.37) is
//! not driven by an attestation exit, and the old instance reaches only the
//! other holders. A failed source check's age is a flag. The model delivers
//! a list in one step, which is page 0's receipt: the lease deadline is fixed
//! there and carried through A4 (H.46). The core profiles
//! check the gate with the modeled holder as the only member: minting stays
//! closed after a supported restore until the holder's current-epoch report,
//! and L1 requires it to reopen when the holder held a high-water at the
//! restore. A holder whose first list arrives after the restore (H1) reopens
//! it with its S4 reset status, which the core profiles start past. For H.19
//! the restored typed evidence is retained separately from live protocol
//! indexes; the real custody bytes and the escalation scheduler are not
//! modeled. Not covered: the H.16 filesystem, walk, grammar, golden-vector,
//! pagination, multi-holder and concurrency vectors (design §5.8, "H.16
//! conformance obligations"), more than two other holders, the modeled
//! holder's own list pagination and cursors, real dual clocks, in-flight query
//! drains, general builders and staging, filesystem/lock failure modes,
//! migration and launcher deployment, the journal's encoding and fsync
//! failures, a raw restore outside the gate fixtures (beyond a synthetic witness), a false
//! old-instance-stopped attestation outside its fixture, the engine-side
//! distribution of capability keys (an open seam item, H.49.4), rate
//! scheduling/cursor churn, R15's time clause, escalation, more than two of
//! anything, or H.11 alert delivery. The S3 conformance gate adds these where
//! Kwiry owns the implementation.

pub mod action;
pub mod bag;
pub mod checker;
pub mod design;
pub mod props;
pub mod row_id;
pub mod rows;
pub mod state;
pub mod step;
pub mod table;

pub mod wire;
