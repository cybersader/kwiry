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
//! - [`props`]: invariants I1–I9 and the liveness goal L1.
//! - [`checker`]: bounded explicit-state BFS with crash injection, plus a
//!   strong-fairness SCC analysis for L1.
//! - [`design`]: revision 8 as written ([`design::Design::REV8`]) and each
//!   repair as a separate [`design::Fix`]; [`design::Design::REV10_1`] has
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
//!   remains). `tests/projection.rs` checks the bisimulation condition (same
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
//! `restore_journal_lost` override (one symbolic flag, not a hash chain), and
//! the H.15 capability window. Not covered: real dual clocks, in-flight query
//! drains, general builders and staging, filesystem/lock failure modes,
//! migration and launcher deployment, the journal's encoding and fsync
//! failures, actual raw restore beyond a synthetic witness, rate
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
