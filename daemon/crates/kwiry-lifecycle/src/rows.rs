//! The §5.8 lifecycle table as data.
//!
//! Bootstrapped once from revision 8's markdown and maintained here since:
//! the model (`step.rs`) is normative, and `table::render` derives the
//! design document's §5.8 from these rows. Every row's id is a [`RowId`]
//! that the transition function cites, so a row cannot exist in the table
//! without a matching id in the model (and `tests/model_check.rs` checks
//! that every row marked [`Model::Step`] is exercised).

use crate::row_id::RowId;

/// How the model covers a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// A transition of the model cites this row.
    Step,
    /// Executed by a focused CI state/fixture rather than required in core profiles.
    Focused(&'static str),
    /// Enforced inside other steps' guards; no transition of its own.
    Implicit(&'static str),
    /// Outside the model's scope (stated in the crate docs).
    NotModeled(&'static str),
}

#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub id: RowId,
    /// State + event, guard, durable writes, next, crash after write,
    /// implements.
    pub cells: [&'static str; 6],
    pub model: Model,
}

#[derive(Clone, Copy, Debug)]
pub struct Machine {
    pub key: &'static str,
    pub title: &'static str,
    pub intro: &'static str,
    pub header: [&'static str; 7],
    pub rows: &'static [Row],
}

macro_rules! row {
    ($id:ident, $a:expr, $b:expr, $c:expr, $d:expr, $e:expr, $f:expr, $m:expr) => {
        Row {
            id: RowId::$id,
            cells: [$a, $b, $c, $d, $e, $f],
            model: $m,
        }
    };
}

use Model::*;

pub const MACHINES: &[Machine] = &[
    Machine {
        key: "M1",
        title: r#"Startup phases"#,
        intro: r#"States: `P0` start → `P0L` locks held → `P1` holder loaded → `P2` started → `P3` inspected → `P3a` markers settled → `P3b` upgrade marker settled → `P4` narrowed → `P5` listening → `P6` classified → `P7` running. There is also `FC(reason)`, fail-closed. Rows that leave the same state are mutually exclusive by guard."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                S1,
                r#"P0, process start"#,
                r#"Configuration valid (§5.1: unique `room_id`, one `tenant_id` per holder)"#,
                r#"—"#,
                r#"P0, then S1L; otherwise `FC(config_invalid)`"#,
                r#"—"#,
                r#"§5.1, HW §14 Q5"#,
                NotModeled("configuration is fixed and valid")
            ),
            row!(
                S1L,
                r#"P0, after S1"#,
                r#"Acquire `holder.lock`, then `DataRootLock`, **before any holder-state read, loss classification or mutation**. Abandoned probes are removed by the lock"#,
                r#"—"#,
                r#"P0L. If either lock is held elsewhere: exit `lock_held`, with nothing read or written"#,
                r#"— (locks are released by the kernel)"#,
                r#"R7-1"#,
                NotModeled(
                    "one process per data root; lock exclusion is a conformance-test concern"
                )
            ),
            row!(
                S2,
                r#"P0L, load identity"#,
                r#"`holder.json` and `holder.key` readable and valid; under both locks inspect `holder-id` without following links; a populated root must already be bound to this holder"#,
                r#"—"#,
                r#"S3, S4 or S5; otherwise `FC(not_enrolled)`"#,
                r#"—"#,
                r#"HW §2.1"#,
                NotModeled("enrollment and key loss")
            ),
            row!(
                S3,
                r#"P0L, `holder-state.json` present"#,
                r#"Read under both locks; parses; schema 3; `holder_id` equals `holder.json` and any root binding; a populated root cannot be adopted under a different holder"#,
                r#"—"#,
                r#"P1. A parse or schema failure gives `FC(holder_state_corrupt)`; a mismatch gives `FC(holder_identity_mismatch)`"#,
                r#"—"#,
                r#"F5-6, R7-1"#,
                Implicit(
                    "boot validates the durable root and state identities; sequential swap fixture"
                )
            ),
            row!(
                S4,
                r#"P0L, `holder-state.json` absent"#,
                r#"No root binding and data root proven lost (§5.2 scan) while both locks are held. **Immediately before W, the file is re-checked as absent and the scan is re-run.** If either re-check fails, take S3 or S5 instead"#,
                r#"Fsync root holder marker before governed publication; W (fresh state: `high_water: null`, `last_list_ref: null`, `reset_pending: true`)"#,
                r#"P1"#,
                r#"Restart takes S3 with `reset_pending` still set, so trust on first use continues"#,
                r#"HW §13, R7-1"#,
                Focused("empty-root reset fixture; two-file persistence cuts are conformance work")
            ),
            row!(
                S5,
                r#"P0L, `holder-state.json` absent"#,
                r#"Data root intact or holder marker present; missing state is never a re-enrollment signal"#,
                r#"—"#,
                r#"`FC(holder_state_missing)`"#,
                r#"—"#,
                r#"R6-6, F5-6"#,
                Implicit("boot refusal under missing state fixture")
            ),
            row!(
                S6,
                r#"P1"#,
                r#"—"#,
                r#"— (start the writer and the publisher actor), then K1 or K1i"#,
                r#"P2"#,
                r#"—"#,
                r#"§4.3"#,
                Step
            ),
            row!(
                S7,
                r#"P2"#,
                r#"Read-only read of markers and pointers; choose the §4.10 matrix row"#,
                r#"—"#,
                r#"P3; a fail-closed row gives `FC(migration)`"#,
                r#"—"#,
                r#"F5-5"#,
                Step
            ),
            row!(
                S8,
                r#"P3"#,
                r#"`tombstones ≠ ∅` and (`rollback.json` absent or `current.v2.json` present)"#,
                r#"M"#,
                r#"P3a"#,
                r#"Restart re-evaluates; S8n applies once M is durable"#,
                r#"§4.10, R4-7"#,
                Step
            ),
            row!(
                S8n,
                r#"P3"#,
                r#"`tombstones = ∅`, or (`rollback.json` present and `current.v2.json` absent): markers unnecessary or already durable"#,
                r#"—"#,
                r#"P3a"#,
                r#"—"#,
                r#"R7-4"#,
                Step
            ),
            row!(
                S9,
                r#"P3a"#,
                r#"Matrix row `switching` with pointer v3"#,
                r#"`upgrade.json = published` (temporary file, fsync, rename, fsync root)"#,
                r#"P3b"#,
                r#"Restart: the row is now `published`, so S9n"#,
                r#"§4.10"#,
                NotModeled("layout-v2 migration")
            ),
            row!(
                S9n,
                r#"P3a"#,
                r#"Any other non-fail-closed matrix row"#,
                r#"—"#,
                r#"P3b"#,
                r#"—"#,
                r#"R7-4"#,
                NotModeled("layout-v2 migration")
            ),
            row!(
                S10,
                r#"P3b"#,
                r#"The v3 layout maps a tombstoned resource, or the withheld map holds a unit reserved in `instructions`"#,
                r#"G (without them) → L3 revoke (`reserved_by` for reserved units; O13 ledger marks for reserved quarantine entries)"#,
                r#"P4"#,
                r#"Restart: S10n applies once G is durable"#,
                r#"§5.3, §5.5, R6-1"#,
                Step
            ),
            row!(
                S10n,
                r#"P3b"#,
                r#"Neither condition holds: narrowing unnecessary or already durable"#,
                r#"— (L3: mark the objects of tombstoned resources and the reserved units `revoked` from their durable reasons; re-apply any reserved ledger mark missing after a crash between L1's W and its ledger write)"#,
                r#"P4"#,
                r#"Restart repeats S10n"#,
                r#"R7-4"#,
                Step
            ),
            row!(
                S11,
                r#"P4"#,
                r#"—"#,
                r#"—"#,
                r#"P5: listener bound; every governed request refuses `resource_unavailable`"#,
                r#"—"#,
                r#"§5.3"#,
                Step
            ),
            row!(
                S12,
                r#"P5, a verified list accepted by H1, H2, H6 or H7"#,
                r#"As in that H row"#,
                r#"A1–A6"#,
                r#"P6"#,
                r#"As in A1–A3"#,
                r#"F5-5"#,
                Step
            ),
            row!(
                S13,
                r#"P6"#,
                r#"—"#,
                r#"—"#,
                r#"P7: enables the matrix's recovery actions, staging resume (listed active and leased only), GC, M6, M7 and M8"#,
                r#"—"#,
                r#"F5-5"#,
                Step
            ),
            row!(
                S14,
                r#"`FC(*)`, any event except S15–S17, `holder init` or re-enrollment"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged: no list, serve, build, publish or delete"#,
                r#"—"#,
                r#"F5-6"#,
                NotModeled("fail-closed startup and offline recovery")
            ),
            row!(
                S15,
                r#"`FC(holder_state_missing)`, operator `recover-state --from-backup` (daemon stopped)"#,
                r#"The command holds `holder.lock` and `DataRootLock`; the backup's `holder_id` equals `holder.json`; the backup's `committed_at > now − retention_window`"#,
                r#"W[copied side files] → W (`holder-state.json` last)"#,
                r#"Restart, then S3"#,
                r#"A partial copy without `holder-state.json` gives S5 again, and the operator reruns"#,
                r#"HW §14 G (backup rule), R7-1"#,
                Focused(
                    "interrupted identity-preserving recovery; actual backup files are not modeled"
                )
            ),
            row!(
                S16,
                r#"`FC(holder_state_missing)`, operator `recover-state --discard-data-root`"#,
                r#"The command holds both locks; `holder-state.json` absent"#,
                r#"D over the data root's contents"#,
                r#"Restart, then S4 (or S5 if anything remains)"#,
                r#"Remaining content gives S5, and the operator reruns"#,
                r#"CONTRACT §1.1, R6-6"#,
                NotModeled("fail-closed startup and offline recovery")
            ),
            row!(
                S17,
                r#"`FC(holder_state_corrupt` or `holder_identity_mismatch)`, operator `--set-aside-corrupt`"#,
                r#"The command holds both locks"#,
                r#"Rename to `holder-state.corrupt-<t>`, fsync the directory"#,
                r#"Restart, then S4 or S5"#,
                r#"Either name, and the next start classifies it"#,
                r#"F5-6"#,
                NotModeled("fail-closed startup and offline recovery")
            ),
            row!(
                S18,
                r#"P5–P7, shutdown"#,
                r#"—"#,
                r#"— (cancel F-rows and report and status transports, R2c and T5; builders stop at a yield; any in-progress W completes)"#,
                r#"Exit"#,
                r#"—"#,
                r#"§4.4"#,
                Implicit("a crash or operator stop discards all volatile state and the transport")
            ),
        ],
    },
    Machine {
        key: "M2",
        title: r#"Holder epoch and high-water"#,
        intro: r#"States: `N` (`high_water: null`), `B(E,s)` (baseline), `A(E,s→E')` (one armed acceptance). The event `L(E_L,s_L)` is a list that passed F3."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                H1,
                r#"N + L"#,
                r#"— (first contact, or trust on first use after S4)"#,
                r#"A1–A6, with W (`hw = (E_L,s_L)`, tombstones ∪)"#,
                r#"B(E_L,s_L)"#,
                r#"As in A1"#,
                r#"HW §3.7, §13"#,
                Step
            ),
            row!(
                H2,
                r#"B(E,s) + L, `E_L = E`, `s_L ≥ s`"#,
                r#"—"#,
                r#"A1–A6, with W (`hw = (E,s_L)`)"#,
                r#"B(E,s_L)"#,
                r#"As in A1"#,
                r#"HW §3.7"#,
                Step
            ),
            row!(
                H3,
                r#"B or A + L, `E_L = E`, `s_L < s`"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; L is never cited; `lifecycle_regressed`"#,
                r#"—"#,
                r#"HW §3.7, H3"#,
                NotModeled("needs a same-epoch rollback (raw restore), the stated residual")
            ),
            row!(
                H4,
                r#"B(E,s) + L, `E_L ≠ E`"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; `unexpected_epoch`"#,
                r#"—"#,
                r#"HW §5, §14 Q4"#,
                Step
            ),
            row!(
                H5,
                r#"B or A, operator `accept-epoch E'` (daemon stopped)"#,
                r#"The command holds `holder.lock`; `E' ≠ E`; `hw ≠ null`"#,
                r#"W (`epoch_acceptance = {from: (E,s), to: E'}`; any earlier one goes to `epoch_history`)"#,
                r#"A(E,s→E')"#,
                r#"Either before or after"#,
                r#"HW §14 G.4, R6-5"#,
                Step
            ),
            row!(
                H6,
                r#"A(E,s→E') + L, `E_L = E'`"#,
                r#"`hw` equals `from`"#,
                r#"A1–A6, with W (`hw = (E',s_L)`; acceptance consumed into `epoch_history`; R14 and a durable fresh-status trigger in the same commit)"#,
                r#"B(E',s_L); T1 at the first opportunity, whatever the count"#,
                r#"Either A(E,s→E') or B(E',s_L) with the fresh-status trigger"#,
                r#"R6-5, R6-2, HW §15 H.19"#,
                Step
            ),
            row!(
                H7,
                r#"A(E,s→E') + L, `E_L = E`, `s_L ≥ s`"#,
                r#"—"#,
                r#"A1–A6, with W (`hw = (E,s_L)`; acceptance voided into `epoch_history`)"#,
                r#"B(E,s_L); `epoch_acceptance_stale`"#,
                r#"As in A1"#,
                r#"R6-5"#,
                NotModeled(
                    "needs the authority to serve the old epoch again after a supported restore"
                )
            ),
            row!(
                H8,
                r#"A(E,s→E') + L, `E_L ∉ {E, E'}`"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; `unexpected_epoch`"#,
                r#"—"#,
                r#"R6-5"#,
                NotModeled("needs a third epoch; the model has two")
            ),
            row!(
                H9,
                r#"N, or `E' = E`, operator `accept-epoch`"#,
                r#"—"#,
                r#"—"#,
                r#"Refused (nothing to accept)"#,
                r#"—"#,
                r#"R6-5"#,
                Implicit("the operator arms only a different, alerted epoch")
            ),
            row!(
                H10,
                r#"Any state + `503 authority_epoch_mismatch` on any route"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; `authority_rollback_suspected`; leases run down; back off"#,
                r#"—"#,
                r#"HW §14 F, F5-7"#,
                Step
            ),
            row!(
                H11,
                r#"A, operator `accept-epoch --cancel` (daemon stopped)"#,
                r#"The command holds `holder.lock`"#,
                r#"W (acceptance voided into `epoch_history`)"#,
                r#"B(E,s)"#,
                r#"Either"#,
                r#"R6-5"#,
                NotModeled("cancelling an armed acceptance is never needed for progress")
            ),
            row!(
                H12j,
                r#"Any state + an authority restore whose retirement journal or replica fails verification (an entry of any kind with a missing or ill-typed member, an unknown `kind` or `v`, or a broken chain)"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged: no partial replay, no new epoch, no capability minted; every route answers `503 authority_epoch_mismatch` (H10) and leases run down"#,
                r#"—"#,
                r#"HW §15 H.1.5, H.14"#,
                Focused("corrupt-journal restore fixture")
            ),
            row!(
                H13j,
                r#"After H12j, the authority's audited `restore_journal_lost` decision"#,
                r#"An admin decision, recorded as `cause: "restore-journal-lost"` with an unsampled security event"#,
                r#"—"#,
                r#"A new epoch without replay, refused as in H4 until `accept-epoch`. Outstanding instructions are superseded and unlisted; audited re-approval needs a new-epoch report. `blocked` returns to quarantined with escalation resumed; status is not current and its floor restarts. Evidence stays in custody; obligations, phases and `last_accepted` stay intact"#,
                r#"—"#,
                r#"HW §15 H.1.5, H.14, H.19"#,
                Focused("audited journal-lost override fixture")
            ),
            row!(
                H14,
                r#"Authority: a new epoch (supported restore, `restore_journal_lost`, or operator)"#,
                r#"Until every enrolled, enabled engine holder of the custody id-space has had a status or obligation report accepted whose `list_ref` is a tag-valid list of the current epoch; disabled or decommissioned holders do not count"#,
                r#"—"#,
                r#"OpenClast mints no search capability for any room of the id-space. A holder still on an earlier-epoch list is served only under capabilities minted before minting stopped: at most 90 s after the stop (H.15, H.21), whatever remains of its 900 s lease. Search for the id-space stays down until its last holder reports (H6 and the T1 fresh status clear it promptly), then minting resumes"#,
                r#"—"#,
                r#"HW §15 H.25, H.1.4, H.15.1, H.19.5, G.4"#,
                Step
            ),
        ],
    },
    Machine {
        key: "M3",
        title: r#"List fetch"#,
        intro: r#"States: `Idle`, `InFlight(nonce, pages, deadlines)`."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                F1,
                r#"Idle + trigger: startup, an interval in `[0.8, 1.0] × relist_seconds`, a rebuild, the step-3a early relist, or a backoff timer"#,
                r#"Nothing in flight; M10 is `Normal`; HW §15 H.8: ≥ 5 s between counted starts; 410 restart and the single replay/expiry retry exempt; echo durable `last_list_ref` except on first list or reset"#,
                r#"—"#,
                r#"InFlight, with a fresh nonce and deadlines of 10 s (connect), 30 s (each response) and 120 s baseline, scaled from page 0 (whole list; §5.3). An unverifiable well-formed `last_list_ref` is ignored and audited; never a list-route 409"#,
                r#"—"#,
                r#"HW §3.1, §7, §15 H.17, amendment B, R6-8"#,
                Step
            ),
            row!(
                F2,
                r#"InFlight + a page that passes HW §3.8, with more pages to come"#,
                r#"Same `list_id`, `rev`, nonce, digest and tag as page 0"#,
                r#"—"#,
                r#"InFlight"#,
                r#"—"#,
                r#"HW §3.7, §3.8"#,
                Implicit("pages are collapsed: a list is verified whole")
            ),
            row!(
                F3,
                r#"InFlight + last page verified"#,
                r#"HW §3.7 consistency; Kwiry's item checks; `instructions[]` only on page 0"#,
                r#"—"#,
                r#"Idle; L goes to M2"#,
                r#"—"#,
                r#"HW §3.7, §14 D"#,
                Step
            ),
            row!(
                F4,
                r#"InFlight + any verification failure"#,
                r#"—"#,
                r#"—"#,
                r#"Idle; all pages discarded; `lifecycle_list_invalid`; back off"#,
                r#"—"#,
                r#"HW §3.8"#,
                Implicit(
                    "a list that fails verification is discarded like a transient failure (F7)"
                )
            ),
            row!(
                F5,
                r#"InFlight + `410 list_expired`"#,
                r#"Fewer than 5 consecutive"#,
                r#"—"#,
                r#"InFlight with a fresh nonce from `cursor: null`. At 5: Idle, back off, `lifecycle_list_livelock`"#,
                r#"—"#,
                r#"HW §3.7"#,
                Implicit("`410` restarts the fetch like a transient failure (F7)")
            ),
            row!(
                F6,
                r#"InFlight + deadline expiry, or cancellation"#,
                r#"—"#,
                r#"—"#,
                r#"Idle; connection closed; partial pages discarded; slot freed; back off"#,
                r#"—"#,
                r#"R6-8"#,
                Step
            ),
            row!(
                F7,
                r#"InFlight + network failure, `429`, or a `503` other than H10"#,
                r#"—"#,
                r#"—"#,
                r#"Idle; back off (from 10 s, capped at `relist_seconds / 2`)"#,
                r#"—"#,
                r#"HW §4.5"#,
                Step
            ),
            row!(
                F8,
                r#"InFlight + `401 invalid_holder_proof` or `403`"#,
                r#"—"#,
                r#"—"#,
                r#"Idle; N1 for `401`, N2 for `403`"#,
                r#"—"#,
                r#"HW §4.5, R7-7"#,
                Step
            ),
            row!(
                F8r,
                r#"InFlight + `401 replay`"#,
                r#"—"#,
                r#"—"#,
                r#"Idle; F1 retries at once with a fresh `jti` (HW §4.5: one retry; a second consecutive `replay` backs off as F7). Never a halt"#,
                r#"—"#,
                r#"HW §4.5, round-8 #6"#,
                Step
            ),
            row!(
                F8p,
                r#"list receives `401 proof_expired`"#,
                r#"First consecutive expiry"#,
                r#"—"#,
                r#"Re-sign the same business request with fresh `jti`/`iat`, `exp = iat + 300`; retry once, exempt from rate"#,
                r#"Restart does not turn an expiry into authentication failure"#,
                r#"HW §15 H.3, H.4, H.8"#,
                Step
            ),
            row!(
                F8b,
                r#"list receives `401 proof_expired`"#,
                r#"Single re-signed retry also expired"#,
                r#"—"#,
                r#"Back off and alert on clock; no authorization halt"#,
                r#"Restart does not turn an expiry into authentication failure"#,
                r#"HW §15 H.3, H.4, H.8"#,
                Focused("consecutive expiry fixture")
            ),
            row!(
                F10,
                r#"Authority artifact has unknown `kid`"#,
                r#"No valid advancing key-set statement under a currently pinned key after one fetch"#,
                r#"—"#,
                r#"Refuse `unknown_kid`; do not apply; leases continue to run"#,
                r#"—"#,
                r#"HW §15 H.6"#,
                Focused("key-rotation fixture")
            ),
            row!(
                F11,
                r#"Authority sends a new key-set statement"#,
                r#"Verify `openclast-lifecycle-keyset+jwt`, issuer, wildcard audience, pinned signer, strictly increasing `keyset_seq`, key and retirement claims"#,
                r#"W (`holder.json`, single writer)"#,
                r#"Accept replacement pins durably before using a new signing key"#,
                r#"Reload the last durable key-set sequence and pins"#,
                r#"HW §15 H.6"#,
                Focused("key-rotation fixture")
            ),
            row!(
                F9,
                r#"InFlight + another trigger"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged (coalesced)"#,
                r#"—"#,
                r#"Amendment B"#,
                Implicit("F1 requires nothing in flight")
            ),
        ],
    },
    Machine {
        key: "M4a",
        title: r#"Apply transaction (inside the publisher actor)"#,
        intro: r#""#,
        header: [
            r#"ID"#,
            r#"Step"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                A0,
                r#"Admission: an H row admitted the verified list, before A1"#,
                r#"—"#,
                r#"L3 only: every resource the list does not show `active`, or that is tombstoned, is refused for serving at once (`pin_and_lease` and the end-of-request check read the refusal). Nothing is revoked or written"#,
                r#"A1"#,
                r#"Nothing durable; restart serves nothing until S12"#,
                r#"Round-8 #3"#,
                Step
            ),
            row!(
                A1,
                r#"Commit the list"#,
                r#"The H row admitted it"#,
                r#"W (`apply_list`: tombstones ∪; `hw`; the H6/H7 acceptance change; R14 where H6)"#,
                r#"A2 or A2n. If the W fails (K2x), the transaction aborts; A0's refusals stay in force until a later apply reaches A4"#,
                r#"Restart: S8 and S10 enforce the tombstones; leases are not persisted, so nothing is served until the next list"#,
                r#"HW §3.4 rule 6, §3.7"#,
                Step
            ),
            row!(
                A2,
                r#"Markers needed"#,
                r#"`tombstones ≠ ∅` and (`rollback.json` absent or `current.v2.json` present)"#,
                r#"M"#,
                r#"A3 or A3n. On K2x: aborted as in A1"#,
                r#"Restart: S8"#,
                r#"R4-7"#,
                Step
            ),
            row!(
                A2n,
                r#"Markers unnecessary or already durable"#,
                r#"`tombstones = ∅`, or (`rollback.json` present and `current.v2.json` absent)"#,
                r#"—"#,
                r#"A3 or A3n"#,
                r#"—"#,
                r#"R7-4"#,
                Step
            ),
            row!(
                A3,
                r#"Publish the classification"#,
                r#"The new layout, withheld map, attribution or reservations differ from the current generation"#,
                r#"G (layout = active and lapsed; withheld = unknown and active-unconfigured, with attribution; obligated, retired and tombstoned dropped; reserved units excluded)"#,
                r#"A4. On K2x: aborted as in A1"#,
                r#"Restart: the generation is durable; S10 or S10n is consistent"#,
                r#"§5.3, R6-1"#,
                Step
            ),
            row!(
                A3n,
                r#"Classification unchanged"#,
                r#"Layout, withheld map, attribution and reservations all equal the current generation (for example a lease-only renewal)"#,
                r#"—"#,
                r#"A4"#,
                r#"—"#,
                r#"R7-4, R6-7"#,
                Step
            ),
            row!(
                A4,
                r#"Swap the snapshot"#,
                r#"—"#,
                r#"W (last_list_ref) → L3 (pointer, leases, classes, inventory, reservations; clear A0's refusals; bump `lease_ver`, `class_ver[r]`, `res_ver[r]`, `obl_ver[o]`, `inv_ver`, `layout_ver` as affected)"#,
                r#"A5"#,
                r#"—"#,
                r#"F5-4, R6-7"#,
                Step
            ),
            row!(
                A5,
                r#"Revoke"#,
                r#"—"#,
                r#"L3 (revoke the objects of obligated, retired and tombstoned resources, O4; an object GC had revoked passes to the obligation, O5; cache-only eviction for withheld ones, O3)"#,
                r#"A6"#,
                r#"—"#,
                r#"§4.3"#,
                Step
            ),
            row!(
                A6,
                r#"Queue work"#,
                r#"—"#,
                r#"—"#,
                r#"Done; M6, M7 and M8 queues rebuilt from L"#,
                r#"—"#,
                r#"§5.6"#,
                Step
            ),
        ],
    },
    Machine {
        key: "M4b",
        title: r#"Resource class"#,
        intro: r#"Writes happen through A1–A6 unless stated. "Configured" and "tombstoned" are read from the configuration and from W."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                C1,
                r#"absent, unknown, active-unconfigured, lapsed or active + L lists `r` `active`"#,
                r#"Configured; not tombstoned; units reserved by an outstanding release are excluded"#,
                r#"A3 maps `r`'s live, unreserved objects into the layout. If there are none, `r` is active and unbuilt, and the builder may build it under K3"#,
                r#"active"#,
                r#"As in A1–A3"#,
                r#"HW §3.4 rule 2, R6-1"#,
                Step
            ),
            row!(
                C2,
                r#"active + lease deadline passes"#,
                r#"`lease_ver` timer"#,
                r#"— (L3)"#,
                r#"lapsed"#,
                r#"—"#,
                r#"Q4, HW §3.3"#,
                Focused("abstract deadline boundary, including during apply")
            ),
            row!(
                C3,
                r#"lapsed + L `active` with a later `quarantine_after`"#,
                r#"—"#,
                r#"A1 (hw only) → L3"#,
                r#"active"#,
                r#"—"#,
                r#"Q4"#,
                Implicit(
                    "a fresh verified still-live lease applied at A4 restores serving; focused fixture"
                )
            ),
            row!(
                C4,
                r#"active, lapsed or active-unconfigured + L does not list `r`"#,
                r#"Not tombstoned"#,
                r#"A3 (layout → withheld) → A5 cache-only eviction"#,
                r#"unknown"#,
                r#"As in A3"#,
                r#"HW §3.4 rule 1, amendment C"#,
                Step
            ),
            row!(
                C5,
                r#"unknown + L `active`"#,
                r#"Configured; units not reserved"#,
                r#"A3 (withheld → layout; objects still live)"#,
                r#"active"#,
                r#"As in A3"#,
                r#"F5-3"#,
                Step
            ),
            row!(
                C6,
                r#"any non-tombstoned + L `terminating`, or `retired` with `escalated`"#,
                r#"—"#,
                r#"A1 (tombstone) → A3 (drop) → A5 (revoke)"#,
                r#"obligated"#,
                r#"Restart: S10"#,
                r#"HW §3.4 rule 3, §14 Q8"#,
                Step
            ),
            row!(
                C7,
                r#"any + L `retired` with `complete`"#,
                r#"—"#,
                r#"A1 → A3 → A5, if objects remain"#,
                r#"retired-complete"#,
                r#"Restart: S10"#,
                r#"HW §3.4 rule 4"#,
                Step
            ),
            row!(
                C8,
                r#"obligated or retired-complete + L no longer lists `r`"#,
                r#"Remaining units of `r`"#,
                r#"After drain: Q for each remaining unit (unattributed)"#,
                r#"tombstoned-unlisted"#,
                r#"Restart: a quarantine scan rebuilds missing ledger entries as unattributed"#,
                r#"HW §3.3 retention"#,
                Step
            ),
            row!(
                C9,
                r#"tombstoned (any class) + L `active`"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; `tombstone_conflict`"#,
                r#"—"#,
                r#"HW H13"#,
                Focused(
                    "a correct authority never lists a retired id active (HW §15 H.7, H.1 replay); reached by an H.7 violation or the audited journal-loss residual"
                )
            ),
            row!(
                C10,
                r#"active or lapsed + L `active`, but `r` is unconfigured"#,
                r#"—"#,
                r#"A3 (layout → withheld) → A5 cache-only eviction"#,
                r#"active-unconfigured"#,
                r#"As in A3"#,
                r#"§5.3"#,
                NotModeled("every room is configured")
            ),
            row!(
                C11,
                r#"any + `403 holder_not_authorized` on any route (F8, R11, T6a)"#,
                r#"—"#,
                r#"— (L3: nothing servable)"#,
                r#"Unchanged classes, all refused while M10 is `AuthHalted(403)`; leaving it requires N4"#,
                r#"—"#,
                r#"HW §4.5, R7-7"#,
                Step
            ),
            row!(
                C12,
                r#"obligated + L still `terminating` or `retired`"#,
                r#"—"#,
                r#"—"#,
                r#"obligated"#,
                r#"—"#,
                r#"HW §3.4"#,
                Step
            ),
            row!(
                C13,
                r#"unknown + L lists `r` `active`, `r` not configured"#,
                r#"Not tombstoned; units not reserved"#,
                r#"A3 keeps `r` in the withheld map, now **attributed** to the listed holding. In the same A4 swap its units leave the unattributed inventory and `inv_ver` is bumped"#,
                r#"active-unconfigured"#,
                r#"As in A3"#,
                r#"R7-3"#,
                NotModeled("every room is configured")
            ),
            row!(
                C14,
                r#"tombstoned-unlisted + L `terminating`, or `retired` with `escalated`"#,
                r#"—"#,
                r#"A3 (no mapping) → A4: `r`'s quarantined units are re-attributed to the obligation, leave the unattributed inventory, and `inv_ver` is bumped"#,
                r#"obligated"#,
                r#"As in A1–A3"#,
                r#"Completeness, HW §3.4 rule 3"#,
                NotModeled("the authority model never re-terminates a dropped holding")
            ),
            row!(
                C16,
                r#"Signed list contains `terminating` + `blocked`"#,
                r#"H.9 decision by authorized human; only remaining blocker at decision was unattributed quarantine"#,
                r#"W (adopt the list as for `terminating`)"#,
                r#"Obligated, withhold; keep attributed purge/reporting, no deletion authority from `blocked`, no escalation while blocked"#,
                r#"Read tombstones, then re-list"#,
                r#"HW §15 H.9"#,
                Focused("release decline and blocked-list fixture")
            ),
            row!(
                C15,
                r#"retired-complete + L `retired` with `escalated`"#,
                r#"—"#,
                r#"A3 → A5 if objects remain"#,
                r#"obligated (purge and reporting continue)"#,
                r#"As in A3"#,
                r#"Completeness, HW §14 Q8"#,
                NotModeled("escalation is not modeled")
            ),
            row!(
                C17,
                r#"active + admit a governed request, then re-check immediately before releasing results"#,
                r#"`exp − iat ≤ 60 s`; `iat ≤ now + 30 s` at admission. At both cuts: `now ≤ exp + clock_skew_allowance`, no C19 clock hold (cleared only by C20), and no A0 list showing `r` not active. Configured skew must be ≤ 30 s or startup is refused"#,
                r#"—"#,
                r#"Release results only after passing the second cut. For a pre-retirement capability the engine window is at most 90 s; OpenClast's separate T1 re-check can still refuse the whole call"#,
                r#"—"#,
                r#"HW §15 H.15, H.21, H.23"#,
                Focused("capability admission and release fixture")
            ),
            row!(
                C18,
                r#"any + a governed request that fails a C17 check"#,
                r#"—"#,
                r#"—"#,
                r#"Refuse with no results: `capability_expired` past the deadline; refuse a future `iat` or excessive lifetime; C19 refuses while the clock is behind; `resource_unavailable` once A0 shows `r` not active. A supported restore adds no window"#,
                r#"—"#,
                r#"HW §15 H.15, H.21, H.23, K.1"#,
                Focused("capability admission and release fixture")
            ),
            row!(
                C19,
                r#"any + a verified list page or capability shows the holder clock is behind"#,
                r#"`iat − now_recv > 30 s` on any verified page (nonce-bound or not) or capability: transit only makes the engine look ahead, so the lag is at least that"#,
                r#"W (`clock_behind_authority`; the refusal takes effect in memory first)"#,
                r#"Raised: refuse every capability-authorized governed request until C20. List fetches, applies, reports and status continue"#,
                r#"The hold survives restart: a restart is no clock correction"#,
                r#"HW §15 H.21.5, H.23"#,
                Focused("capability admission and release fixture")
            ),
            row!(
                C20,
                r#"raised + a verified list page received in this process in response to a list request this process sent (F1 or C21), with the matching `nonce` echo"#,
                r#"`(iat − now_recv) + rtt ≤ 30 s`, with `rtt` measured on the monotonic clock from sending that page's request to receiving the page. Lag ≤ `(iat − now_recv)` + transit and transit ≤ `rtt`, so this bounds the lag at 30 s"#,
                r#"W (clear `clock_behind_authority`)"#,
                r#"Cleared; governed requests resume under C17. Otherwise unchanged, with an alert naming `measured_lag_upper_bound` and `rtt`. **Nothing else clears the hold:** no capability, no page without this process's nonce, no older or replayed artifact, no NTP or clock-step event, no elapsed time and no operator command"#,
                r#"Either: the hold, or its confirmed clearing"#,
                r#"HW §15 H.23"#,
                Focused("clock-correction fixture")
            ),
            row!(
                C21,
                r#"raised + the probe timer (at most once per 60 s)"#,
                r#"M10 Normal"#,
                r#"—"#,
                r#"One extra list request with `page_size: 1` and a fresh `nonce`, counted under H.8. Its page feeds C20; its result need not be applied. With clocks that agree and a round trip under 30 s it confirms; otherwise the hold stays and alerts (fail closed)"#,
                r#"The request dies with the process; the next process probes again"#,
                r#"HW §15 H.23, H.8"#,
                Focused("clock-correction fixture")
            ),
        ],
    },
    Machine {
        key: "M5",
        title: r#"Object lifetime"#,
        intro: r#"States: `live`, `revoked`, `doomed`, `deleted`, and `quarantined`. A quarantine entry's owner is derived from its descriptor's current class (GC, an obligation `o`, or unattributed), except a release pin `reserved_by = id`, which is stored."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                O1,
                r#"(none) + build commit"#,
                r#"Build root; admission token; K3"#,
                r#"§4.2 steps 1–3 (`sync_tree`, rename, fsync both parents)"#,
                r#"live (build-root reference)"#,
                r#"Unreferenced, so it becomes garbage (O4(b) or O8)"#,
                r#"§4.2"#,
                NotModeled("builders")
            ),
            row!(
                O2,
                r#"live + publication referencing it"#,
                r#"Publisher revalidation (§4.3)"#,
                r#"G"#,
                r#"live (layout or withheld reference)"#,
                r#"—"#,
                r#"§4.3"#,
                Focused("one late builder output, not the full builder state machine")
            ),
            row!(
                O3,
                r#"live + withholding (C4, C10)"#,
                r#"—"#,
                r#"G → L3 cache-only eviction"#,
                r#"live"#,
                r#"—"#,
                r#"F5-3"#,
                Implicit("cache-only eviction has no state the properties read")
            ),
            row!(
                O4,
                r#"live + revoke reason: (a) obligated or retired drop (A5); (b) unreferenced and attributed to an active resource (GC); (c) release reservation (L1)"#,
                r#"The reason is already durable: W tombstone, G, or W reservation"#,
                r#"L3 (`revoked`, cell detached, reader dropped outside L3)"#,
                r#"revoked"#,
                r#"Restart re-derives `revoked` from the durable reason (S10, GC scan)"#,
                r#"§4.3, R4-1"#,
                Step
            ),
            row!(
                O5,
                r#"revoked + claim"#,
                r#"Lease, handle and init counts all zero; no layout, withheld, retained or build-root reference; the claimant owns the reason (obligation for `r`, GC, or the release that reserved it). **GC claims only while `r` is still active in the current snapshot**; otherwise the object stays revoked and A5 (obligated) or O8 (unknown, tombstoned-unlisted) takes it"#,
                r#"— (L3)"#,
                r#"doomed"#,
                r#"—"#,
                r#"§4.3, R6-1, model M-1"#,
                Step
            ),
            row!(
                O6,
                r#"doomed + delete"#,
                r#"—"#,
                r#"D"#,
                r#"deleted"#,
                r#"A partial tree remains with its reason still durable, so O4–O6 repeat"#,
                r#"§5.6 step 5"#,
                Step
            ),
            row!(
                O7,
                r#"doomed + delete failure"#,
                r#"—"#,
                r#"Q. The ledger entry keeps the descriptor and, for a reserved unit, the pin `reserved_by = id` (O13). Its owner is **derived** at every L3 update from the descriptor's current class: active → GC (O11g), obligated or retired → the obligation (O11), unknown or tombstoned-unlisted → unattributed"#,
                r#"quarantined (owner derived)"#,
                r#"Restart: the quarantine scan rebuilds the entry from its descriptor and the W reservations; the owner is derived again"#,
                r#"P-b, R7-6, round-8 #7"#,
                Step
            ),
            row!(
                O8,
                r#"live (or revoked by GC whose resource is no longer active, O5), unreferenced, attributed to an unknown, tombstoned-unlisted or unattributable resource"#,
                r#"—"#,
                r#"L3 revoke → drain → Q"#,
                r#"quarantined (unattributed)"#,
                r#"As in O7"#,
                r#"§4.5, amendment C"#,
                Step
            ),
            row!(
                O9,
                r#"revoked or doomed + lease or reader-init request"#,
                r#"—"#,
                r#"—"#,
                r#"Refused (pin retries, then `index_building`)"#,
                r#"—"#,
                r#"F1, R4-1"#,
                Implicit("`servable()` refuses revoked and doomed objects")
            ),
            row!(
                O10,
                r#"live (withheld only) + lease request"#,
                r#"Resource not servable"#,
                r#"—"#,
                r#"Refused (`resource_unavailable`)"#,
                r#"—"#,
                r#"§5.4"#,
                Implicit("`servable()` refuses withheld resources")
            ),
            row!(
                O11,
                r#"quarantined (owner derived: obligation `o`) + retry timer"#,
                r#"`o` is still listed (obligated or retired-complete); no registry entry, lease or handle exists for the entry; no process fd refers to it"#,
                r#"D"#,
                r#"deleted. On failure: unchanged, backoff"#,
                r#"Partial delete: O11 retries"#,
                r#"R7-6"#,
                Step
            ),
            row!(
                O11g,
                r#"quarantined (owner derived: GC; the descriptor's resource is listed `active`) + retry timer"#,
                r#"The resource is still active; no registry entry, lease or handle exists for the entry; no process fd refers to it"#,
                r#"D"#,
                r#"deleted. On failure: unchanged, backoff"#,
                r#"Partial delete: O11g retries"#,
                r#"§4.5, round-8 #7"#,
                Step
            ),
            row!(
                O12,
                r#"quarantined (`reserved_by = id`) + L5 drain step"#,
                r#"Instruction `id` is `draining`; no process fd refers to the entry"#,
                r#"D"#,
                r#"deleted. On failure: unchanged, L5f"#,
                r#"Partial delete: L5 retries"#,
                r#"R7-6"#,
                Step
            ),
            row!(
                O13,
                r#"quarantined (unattributed) + L1 reservation"#,
                r#"L1's guard passed for this unit"#,
                r#"Ledger mark `reserved_by = id` (ledger update, fsync), after L1's W"#,
                r#"quarantined (`reserved_by = id`): the quarantined counterpart of O4(c)"#,
                r#"A crash between L1's W and the ledger write is repaired by S10 or S10n"#,
                r#"R7-6"#,
                Step
            ),
            row!(
                O14x,
                r#"Independent census discovers a late builder output after final scan"#,
                r#"Object belongs to the obligated resource"#,
                r#"— (model); production republishes registry and durably reconciles any staged files"#,
                r#"Invalidate the scan and complete attempt; re-admit only under the purge claimant"#,
                r#"Repeat physical census after restart"#,
                r#"Astra deletion evidence"#,
                Focused("late-publication fixture")
            ),
            row!(
                O14,
                r#"quarantined (owner derived: obligation `o`) + `o` leaves the list (C8, R12)"#,
                r#"—"#,
                r#"— (the derived owner becomes unattributed in the A4 swap; `inv_ver` is bumped)"#,
                r#"quarantined (unattributed)"#,
                r#"—"#,
                r#"R7-6, round-8 #7"#,
                Implicit("derived in the A4 swap; no transition of its own")
            ),
        ],
    },
    Machine {
        key: "M6",
        title: r#"Obligation report (per obligation `o`)"#,
        intro: r#"The floor is `F = floors[o]` in the current epoch. Pending states: `none`, `ready`, `armed` (attempt durable, not yet handed off), `queued` (handed to the transport, not yet on the wire; volatile), `sent` (on the wire; volatile, so a restart restores `armed`), `invalidated`, `rejected`, `halted`. The transmission guard and the enqueue guard are defined in §5.6."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                R1,
                r#"none, invalidated or rejected (or ready or sent with a lower state) + a derived state `s`"#,
                r#"Publisher transaction: `obl_ver[o]`, `res_ver[r]`, `inv_ver` and the epoch unchanged since the pass. Then **either** `s > F`, **or** `s == F` as a same-state update (counts or manifest digest differ from the last accepted report) or a reconfirmation (R1r, or after R14), at most once per 300 s per obligation. Plus the enqueue guard, and an empty inventory for `complete`. After an epoch acceptance, nothing is transmitted until the H6 fresh-epoch status has a current-epoch receipt (T1 goes first); `429`, `401 replay`, `401 proof_expired`, `409 stale_list_ref` and `503` on the status keep this gate shut while T2 retries. A status bug halt (T6c, or R17 on the status) releases it (H.22)"#,
                r#"W[`receipts/<key>` scan record, for `complete`] → W[`manifests/<key>`] → W (`pending = ready{key, B}`)"#,
                r#"ready"#,
                r#"After a side file: an orphan, later pruned. After W: ready, re-validated by R2 after the restart"#,
                r#"HW §4.1, §4.4, §14 Q3, Q7, §15 H.19, H.22; F5-4; R6-7; R7-5"#,
                Step
            ),
            row!(
                R1r,
                r#"retired-complete whose data reappeared (C7 with artifacts) + a purge pass completes"#,
                r#"As R1, with `s = complete == F` allowed as a reconfirmation"#,
                r#"As R1"#,
                r#"ready (the authority answers `already_complete`, R6)"#,
                r#"As R1"#,
                r#"HW §3.4 rule 4, R7-5"#,
                NotModeled("data never reappears after `complete` (no builders)")
            ),
            row!(
                R2,
                r#"ready, sent, or armed restored by a restart (R4a) + transmission (first send or retry)"#,
                r#"The transmission guard passes against the current snapshot and **records the versions it read** (`obl_ver[o]`, `res_ver[r]`, `inv_ver`, epoch). For `complete` after a restart, a barrier and scan completed in this process"#,
                r#"W[`sent/<d>.jws`] → W (append `{jti, d, exp}`; pending → `armed`)"#,
                r#"armed"#,
                r#"After W: the attempt counts as possibly accepted"#,
                r#"F5-1, R6-3"#,
                Step
            ),
            row!(
                R2h,
                r#"armed + handoff"#,
                r#"A **publisher-actor handoff transaction**, serialized with A4: the versions still equal those R2 recorded"#,
                r#"— (a non-blocking enqueue of the exact bytes to the report transport). **Enqueue is not transmission**: the bytes leave only through R2d"#,
                r#"queued"#,
                r#"—"#,
                r#"R7-2, R7-8"#,
                Step
            ),
            row!(
                R2d,
                r#"queued + dispatch (transport)"#,
                r#"**The linearization point.** In one L3 critical section immediately before the first byte is written: the versions R2 recorded are unchanged, and the attempt has been queued for less than 30 s (well inside its JWS `exp`)"#,
                r#"— (bytes on the wire: connect 10 s, response 30 s, under a cancellation token)"#,
                r#"sent"#,
                r#"—"#,
                r#"Round-8 #2"#,
                Step
            ),
            row!(
                R2q,
                r#"queued + dispatch (transport)"#,
                r#"The versions moved, or the attempt was queued for 30 s or more"#,
                r#"— (the queued attempt is cancelled; nothing was transmitted)"#,
                r#"armed, versions marked moved; then R2x"#,
                r#"Restart: R4a"#,
                r#"Round-8 #2"#,
                Step
            ),
            row!(
                R2x,
                r#"armed + handoff, or after R2q"#,
                r#"The versions differ"#,
                r#"W (pending → invalidated; this attempt is marked `not_transmitted` and pruned with its `sent/` bytes; earlier attempts go to `uncertain`)"#,
                r#"invalidated, then R1"#,
                r#"Restart before W: R4 treats `armed` as possibly transmitted"#,
                r#"R7-2"#,
                Step
            ),
            row!(
                R2c,
                r#"sent + transport cancellation (S18)"#,
                r#"—"#,
                r#"—"#,
                r#"sent, outcome unknown: R4 on restart"#,
                r#"—"#,
                r#"R7-8"#,
                Implicit("a crash or stop discards the transport; R4a resumes")
            ),
            row!(
                R3,
                r#"ready, sent or armed restored by a restart (R4a) + transmission"#,
                r#"The transmission guard fails"#,
                r#"W (`pending → invalidated`; any attempts and their `sent/` bytes move to `uncertain[key]`)"#,
                r#"invalidated, then R1"#,
                r#"Either before or after"#,
                r#"R6-3"#,
                Step
            ),
            row!(
                R4,
                r#"ready, armed or sent, restart"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged. A restored `armed` is *sent, outcome unknown* (R4a). Only R2 or R3 may follow, and only from P7"#,
                r#"—"#,
                r#"F5-5, R7-2, round-8 #1"#,
                Implicit("durable pending reports survive every restart unchanged")
            ),
            row!(
                R4a,
                r#"armed restored by a restart (never handed off in this process) + transmission"#,
                r#"As R2, or as R3 when the transmission guard fails"#,
                r#"As R2 (the restored attempt counts as an earlier attempt that may have been accepted) or as R3 (it moves to `uncertain`)"#,
                r#"armed (R2) or invalidated (R3)"#,
                r#"As R2 or R3"#,
                r#"Round-8 #1"#,
                Step
            ),
            row!(
                R5,
                r#"sent + replay receipt"#,
                r#"Replay path (§5.6)"#,
                r#"W[`receipts/<key>` ack] → W (`F = max(F, s)`; clear pending if `F > pending.state` or the receipt is for the pending key; prune the key's `sent/` and attempts)"#,
                r#"none, or the remaining pending"#,
                r#"After the receipt: the next retry gets the same original ack, so R5 again"#,
                r#"HW §4.3, §4.4, R7-5"#,
                Step
            ),
            row!(
                R6,
                r#"sent or invalidated + convergence receipt (`409 state_regression` body, `200 already_complete`) **whose ack was minted in the current epoch** (ack `rev.epoch` equals the high-water's)"#,
                r#"Convergence path (§5.6)"#,
                r#"W[`receipts/conv-<ack jti>`] → W (`F = max(F, accepted_state)`; clear pending and uncertain entries that are covered)"#,
                r#"none, or pending"#,
                r#"As in R5"#,
                r#"HW §4.4, §15 H.5: `current_ack` problem member or `OpenClast-Lifecycle-Outcome: already_complete`; key mismatch also means convergence"#,
                Step
            ),
            row!(
                R6e,
                r#"sent or invalidated + convergence receipt whose ack was minted in another epoch"#,
                r#"Convergence path; ack `rev.epoch` ≠ the high-water's epoch"#,
                r#"W[`receipts/conv-<ack jti>`] (audit) → W (if it answered the sent report: `pending → rejected`). **No floor change**"#,
                r#"rejected or unchanged; a list must apply before R1 re-derives for `o` (R13 learns the current epoch's floor)"#,
                r#"Either"#,
                r#"R6-2, round-8 #4"#,
                Focused("previous-epoch convergence vector V4")
            ),
            row!(
                R7,
                r#"sent + `409 complete_refused_unattributed` or `409 stale_list_ref`"#,
                r#"Definitively not accepted: HW §4.1 checks idempotency before `list_ref` and before `complete` preconditions"#,
                r#"W[`receipts/<key>` refusal] → W (`pending → rejected`; prune the key's `sent/`)"#,
                r#"rejected, then R1 at any state above F. For `stale_list_ref`, F1 runs first"#,
                r#"Either"#,
                r#"HW §14 Q3, §4.5, R6-2"#,
                Step
            ),
            row!(
                R8,
                r#"sent + `404 obligation_not_found`"#,
                r#"—"#,
                r#"W (`pending → rejected`)"#,
                r#"rejected; F1. If `o` is then absent, R12"#,
                r#"Either"#,
                r#"HW §4.5"#,
                Focused("report-only refusal fixture; supported journal replay keeps identities")
            ),
            row!(
                R9,
                r#"sent + `409 idempotency_conflict`, `400 invalid_request` or `413`"#,
                r#"—"#,
                r#"W (`pending → halted`)"#,
                r#"halted; bug alert; never re-derived automatically. N5 resumes after an operator fix"#,
                r#"Either"#,
                r#"HW §4.5, R7-7"#,
                Step
            ),
            row!(
                R10,
                r#"sent + `429`, a `503`, H.20 `500`, network failure or deadline"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; back off; `authority_epoch_mismatch` also H10. For H.20 the authority emits `lifecycle.invariant_violation` without committing"#,
                r#"—"#,
                r#"HW §4.5, §14 F, §15 H.20"#,
                Step
            ),
            row!(
                R11,
                r#"sent + `401 invalid_holder_proof` or `403`"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; N1 for `401`, N2 for `403`. Never re-derived"#,
                r#"—"#,
                r#"HW §4.5, R7-7"#,
                Step
            ),
            row!(
                R11r,
                r#"sent + `401 replay`"#,
                r#"—"#,
                r#"—"#,
                r#"sent, retry due: R2 retries at once with a fresh `jti` (HW §4.5). Never a halt"#,
                r#"—"#,
                r#"HW §4.5, round-8 #6"#,
                Step
            ),
            row!(
                R11p,
                r#"report receives `401 proof_expired`"#,
                r#"First consecutive expiry"#,
                r#"—"#,
                r#"Re-sign the same business request with fresh `jti`/`iat`, `exp = iat + 300`; retry once, exempt from rate"#,
                r#"Restart does not turn an expiry into authentication failure"#,
                r#"HW §15 H.3, H.4, H.8"#,
                Step
            ),
            row!(
                R11b,
                r#"report receives `401 proof_expired`"#,
                r#"Single re-signed retry also expired"#,
                r#"—"#,
                r#"Back off and alert on clock; no authorization halt"#,
                r#"Restart does not turn an expiry into authentication failure"#,
                r#"HW §15 H.3, H.4, H.8"#,
                Focused("consecutive expiry fixture")
            ),
            row!(
                R12,
                r#"any + a verified list without `o`"#,
                r#"—"#,
                r#"W (pending archived to `receipts/`; `floors[o]` removed; uncertain kept for R15)"#,
                r#"none (ended)"#,
                r#"Either"#,
                r#"HW §3.3 retention"#,
                Step
            ),
            row!(
                R13,
                r#"any + a verified list in which `o.last_accepted = a` and `a > F`"#,
                r#"Same epoch"#,
                r#"W (`F = a`, receipt `list:<list_id>`; clear pending only if `a > pending.state`)"#,
                r#"—"#,
                r#"Either"#,
                r#"HW §3.3, R7-5"#,
                Step
            ),
            row!(
                R14,
                r#"any + H6 (epoch acceptance consumed)"#,
                r#"—"#,
                r#"Inside H6's W: floors go to `floor_history`; pending and status are invalidated (attempts go to `uncertain`)"#,
                r#"none (re-confirmation allowed)"#,
                r#"As in H6"#,
                r#"R6-2, R6-5"#,
                Step
            ),
            row!(
                R15,
                r#"`uncertain[k]` + a verified list"#,
                r#"Either `last_accepted ≥ k.state`, or `list.iat > max(attempt.exp) + 420 s` with `last_accepted < k.state`"#,
                r#"W (prune `uncertain[k]`) → D (`sent/` bytes)"#,
                r#"resolved"#,
                r#"Leftover `sent/` files pruned later"#,
                r#"R6-3"#,
                Step
            ),
            row!(
                R16,
                r#"any + a same-state derivation (`s == F`, or `s` equal to the invalidated or rejected state)"#,
                r#"Less than 300 s since the last same-state report for `o`"#,
                r#"—"#,
                r#"Wait until the window opens, then R1"#,
                r#"—"#,
                r#"HW §14 Q7, R7-5"#,
                NotModeled("rate limits only delay; the model sends as early as allowed")
            ),
            row!(
                R17,
                r#"sent + an ack with a valid signature that matches no path, **or** a replay receipt for a key R7 recorded as rejected"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; `ack_unverifiable` (`ack_contradiction` for a rejected key); after 3 in a row, halted (N5 resumes)"#,
                r#"—"#,
                r#"F5-1"#,
                Focused(
                    "lost-attempt fixture V2; repeated unverifiable and forged acks are not explored"
                )
            ),
            row!(
                R18,
                r#"any pending state (including none) + a replay receipt for a key in `uncertain` (a late receipt after invalidation, arriving outside `sent`)"#,
                r#"Replay path against that key's attempts and body; the key's epoch is current"#,
                r#"W[`receipts/<k>`] → W (`F = max(F, k.state)`; prune `uncertain[k]`; clear the current pending only if `F > pending.state`)"#,
                r#"Pending unchanged or cleared"#,
                r#"After the receipt: idempotent"#,
                r#"Completeness, F5-1"#,
                Step
            ),
            row!(
                R19,
                r#"any + a replay receipt for an `uncertain` key from an earlier epoch"#,
                r#"The key's epoch ≠ the current epoch"#,
                r#"W[`receipts/<k>`] (audit) → W (prune `uncertain[k]`); **no** floor change"#,
                r#"Unchanged"#,
                r#"Either"#,
                r#"R6-2"#,
                Step
            ),
        ],
    },
    Machine {
        key: "M7",
        title: r#"Release instruction (per `instruction_id`)"#,
        intro: r#"States: `unseen`, `reserved`, `draining`, `deleted`, `result_sent`, `done`, `mismatch`, `mismatch_reported`."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                L1,
                r#"unseen + a `quarantine_release` entry on page 0"#,
                r#"A publisher transaction run **after A4** of the list that carried it, against that completed snapshot and its `inv_ver`. It requires: `issued_seq ≤ rev.seq`; a manifest retained for `bound_report_digest`; its count equal to `approved_count`; and **every** manifest unit in the current unattributed inventory, unreserved, with its attribution unchanged"#,
                r#"W (`instructions[id] = reserved{units, digest}`) → G (withheld map without the object units) → ledger marks for quarantined units (O13) → L3 (revoke, `reserved_by = id`; bump `inv_ver`)"#,
                r#"reserved"#,
                r#"After W: S10 or S10n detaches and marks before any list applies. After G: complete"#,
                r#"HW §14 D, G.5, R6-1, R7-3"#,
                Step
            ),
            row!(
                L1d,
                r#"reserved, not detached in this process (L1's G or ledger write failed with K2x)"#,
                r#"Publisher transaction; P7"#,
                r#"G (withheld map without the object units) → ledger marks (O13) → L3 (revoke, `reserved_by = id`; bump `inv_ver`)"#,
                r#"reserved (detached)"#,
                r#"Restart: S10 or S10n detaches before any list"#,
                r#"Model M-2"#,
                Step
            ),
            row!(
                L2,
                r#"unseen + the same entry"#,
                r#"Any L1 guard fails. This **includes a bound unit that has become attributed** (active-unconfigured, active, or obligated: C1, C13, C14)"#,
                r#"W (`instructions[id] = mismatch{observed: current digest}`)"#,
                r#"mismatch, then T1 with `release_result: mismatch`"#,
                r#"Either"#,
                r#"HW §14 G.2, G.5, R6-4, R7-3"#,
                Step
            ),
            row!(
                L3,
                r#"unseen + an entry of unknown `kind`"#,
                r#"—"#,
                r#"—"#,
                r#"Ignored and logged"#,
                r#"—"#,
                r#"HW §14 D"#,
                NotModeled("unknown instruction kinds")
            ),
            row!(
                L4,
                r#"reserved + start drain (outside the actor)"#,
                r#"The reservation is detached in this process (L1, L1d, S10 or S10n)"#,
                r#"W (`draining`)"#,
                r#"draining"#,
                r#"Either"#,
                r#"R6-1"#,
                Step
            ),
            row!(
                L5,
                r#"draining + a unit ready (an object with all counts zero, or a quarantine entry under O12)"#,
                r#"The claimant is `id`"#,
                r#"For each unit: L3 doom → D (objects), or O12 (quarantine entries). When all are gone, W (`deleted`)"#,
                r#"deleted"#,
                r#"Mid-way: S10 or S10n keeps the reservation and L5 resumes; units already deleted are absent"#,
                r#"HW §14 G.5"#,
                Step
            ),
            row!(
                L5f,
                r#"draining + a unit's delete fails"#,
                r#"—"#,
                r#"— (the unit stays revoked and reserved; retried with backoff)"#,
                r#"draining"#,
                r#"—"#,
                r#"P-b"#,
                Step
            ),
            row!(
                L6,
                r#"deleted"#,
                r#"Publisher transaction"#,
                r#"L3 (remove the units from the inventory, bump `inv_ver`) → T1 with `release_result: executed`"#,
                r#"result_sent"#,
                r#"Restart: the inventory is rebuilt from disk, and T1 is re-derived"#,
                r#"HW §14 G.2"#,
                Step
            ),
            row!(
                L7,
                r#"result_sent + T4 for that result"#,
                r#"—"#,
                r#"Inside T4's W: `done`"#,
                r#"done"#,
                r#"As in T4"#,
                r#"HW §14 G.2"#,
                Step
            ),
            row!(
                L8,
                r#"mismatch + T4 for the mismatch result"#,
                r#"—"#,
                r#"Inside T4's W: `mismatch_reported`"#,
                r#"mismatch_reported"#,
                r#"As in T4"#,
                r#"HW §14 G.2"#,
                Step
            ),
            row!(
                L9,
                r#"reserved or draining + L reclassifies a reserved unit's resource"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; reserved units are never re-mapped (C1)"#,
                r#"—"#,
                r#"R6-1"#,
                Implicit("reserved units are excluded from every mapping (`generation.excluded`)")
            ),
            row!(
                L10,
                r#"done or mismatch_reported + still listed"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; T1 only if no accepted status report carries the result"#,
                r#"—"#,
                r#"HW §14 G.2"#,
                Implicit("a listed result is re-reported until an accepted status carries it")
            ),
            row!(
                L11,
                r#"reserved, draining or deleted + no longer listed"#,
                r#"—"#,
                r#"—"#,
                r#"Continue to done"#,
                r#"—"#,
                r#"HW §14 G.5"#,
                Step
            ),
        ],
    },
    Machine {
        key: "M8",
        title: r#"Holder status report"#,
        intro: r#"States: `none`, `ready`, `armed`, `queued`, `sent`, `invalidated`, `rejected`, `halted`, plus `uncertain` keys."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                T1,
                r#"none, invalidated or rejected + a trigger (`reset_pending`, digest change, release outcome, daily while non-zero, or H6 in any epoch transition even at zero)"#,
                r#"New status key: at least 60 s since last new key; same-key retries exempt. Publisher transaction reads `inv_ver`"#,
                r#"W[`manifests/<key>`] → W (`status = ready{key, body}`)"#,
                r#"ready"#,
                r#"Either; an unacknowledged H6 trigger survives restart"#,
                r#"HW §14 E, G.2, §15 H.19"#,
                Step
            ),
            row!(
                T2,
                r#"ready, sent, or armed restored by a restart (T2r) + transmission"#,
                r#"The re-derived count, digest, `holder_state` and `release_result` all equal the body; the versions read are recorded"#,
                r#"W[`sent/<d>.jws`] → W (attempt; → `armed`)"#,
                r#"armed"#,
                r#"As in R2"#,
                r#"HW §14 G.1, R6-3"#,
                Step
            ),
            row!(
                T2r,
                r#"armed restored by a restart + transmission"#,
                r#"As T2, or as T3 when the guard fails"#,
                r#"As T2 (the restored attempt counts as earlier) or as T3 (it moves to `uncertain`)"#,
                r#"armed (T2) or invalidated (T3)"#,
                r#"As T2 or T3"#,
                r#"Round-8 #1"#,
                Step
            ),
            row!(
                T2h,
                r#"armed + handoff"#,
                r#"A publisher-actor handoff transaction, serialized with A4: `inv_ver`, the release-outcome state and `reset_pending` are unchanged"#,
                r#"— (non-blocking enqueue; the bytes leave only through T2d)"#,
                r#"queued"#,
                r#"—"#,
                r#"R7-2, R7-8"#,
                Step
            ),
            row!(
                T2d,
                r#"queued + dispatch (transport)"#,
                r#"The first-byte linearization point: M10 is not `AuthHalted(403)`; `inv_ver`, the release-outcome state and `reset_pending` are unchanged since T2, and the attempt has been queued for less than 30 s"#,
                r#"— (bytes on the wire: connect 10 s, response 30 s, under a cancellation token)"#,
                r#"sent"#,
                r#"—"#,
                r#"Round-8 #2"#,
                Step
            ),
            row!(
                T2q,
                r#"queued + dispatch (transport)"#,
                r#"Something moved, or the attempt was queued for 30 s or more"#,
                r#"— (cancelled; nothing was transmitted)"#,
                r#"armed, versions marked moved; then T2x"#,
                r#"Restart: T2r"#,
                r#"Round-8 #2"#,
                Step
            ),
            row!(
                T2x,
                r#"armed + handoff, or after T2q"#,
                r#"Something changed"#,
                r#"W (`invalidated`; this attempt pruned as `not_transmitted`; earlier ones go to `uncertain`)"#,
                r#"invalidated, then T1"#,
                r#"As in R2x"#,
                r#"R7-2"#,
                Step
            ),
            row!(
                T3,
                r#"ready or sent + transmission"#,
                r#"The guard fails"#,
                r#"W (`invalidated`; attempts go to `uncertain`)"#,
                r#"invalidated, then T1"#,
                r#"Either"#,
                r#"R6-3"#,
                Step
            ),
            row!(
                T4,
                r#"sent + status receipt"#,
                r#"Status path (§5.6)"#,
                r#"W[`receipts/<key>`] → W (status accepted; `reset_pending = false` if the body said reset; clear H6 trigger on a current-epoch receipt; L7 or L8 in the same commit)"#,
                r#"none"#,
                r#"After the receipt: the retry gets the same ack"#,
                r#"HW §14 G.1"#,
                Step
            ),
            row!(
                T5,
                r#"sent + `429`, a `503`, H.20 `500`, network failure, deadline, or transport cancellation (S18)"#,
                r#"—"#,
                r#"—"#,
                r#"Back off, or exit; `authority_epoch_mismatch` also H10. H.20 commits nothing and emits `lifecycle.invariant_violation`"#,
                r#"—"#,
                r#"HW §14 E, F, §15 H.20, R7-8"#,
                Step
            ),
            row!(
                T6a,
                r#"sent + `403 holder_not_authorized`"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; N2 (and C11)"#,
                r#"—"#,
                r#"HW §4.5, R7-7"#,
                Step
            ),
            row!(
                T6b,
                r#"sent + `401 invalid_holder_proof`"#,
                r#"—"#,
                r#"—"#,
                r#"Unchanged; N1"#,
                r#"—"#,
                r#"HW §4.5, R7-7"#,
                Step
            ),
            row!(
                T6r,
                r#"sent + `401 replay`"#,
                r#"—"#,
                r#"—"#,
                r#"sent, retry due: T2 retries at once with a fresh `jti`. Never a halt"#,
                r#"—"#,
                r#"HW §4.5, round-8 #6"#,
                Step
            ),
            row!(
                T6p,
                r#"status receives `401 proof_expired`"#,
                r#"First consecutive expiry"#,
                r#"—"#,
                r#"Re-sign the same business request with fresh `jti`/`iat`, `exp = iat + 300`; retry once, exempt from rate"#,
                r#"Restart does not turn an expiry into authentication failure"#,
                r#"HW §15 H.3, H.4, H.8"#,
                Step
            ),
            row!(
                T6e,
                r#"status receives `401 proof_expired`"#,
                r#"Single re-signed retry also expired"#,
                r#"—"#,
                r#"Back off and alert on clock; no authorization halt"#,
                r#"Restart does not turn an expiry into authentication failure"#,
                r#"HW §15 H.3, H.4, H.8"#,
                Focused("consecutive expiry fixture")
            ),
            row!(
                T6c,
                r#"sent + `409 idempotency_conflict`, `400 invalid_request` or `413`"#,
                r#"—"#,
                r#"W (status → `halted`)"#,
                r#"halted; bug alert; never re-derived automatically. N5 resumes. Releases the R1 status-first gate: obligation reports proceed while the status waits"#,
                r#"Either; a durable `halted` keeps the gate released"#,
                r#"HW §4.5, §15 H.22, R7-7"#,
                Step
            ),
            row!(
                T6d,
                r#"sent + `409 stale_list_ref`"#,
                r#"—"#,
                r#"W (`rejected`)"#,
                r#"rejected; F1, then T1. The status floor is the last accepted status `list_ref` of the current epoch only, so a tag-valid current-epoch `list_ref` never gets this answer from an earlier epoch's floor"#,
                r#"Either"#,
                r#"HW §15 H.4 (no status 404), H.22"#,
                Step
            ),
            row!(
                T7,
                r#"any + a late status receipt for a key in `uncertain`"#,
                r#"Status path against that key's attempts and body"#,
                r#"W[`receipts/<k>`] → W (prune `uncertain[k]`; L7 or L8 if the body carried `release_result`; `reset_pending = false` if the body said reset)"#,
                r#"—"#,
                r#"After the receipt: idempotent"#,
                r#"Completeness"#,
                Step
            ),
        ],
    },
    Machine {
        key: "M9",
        title: r#"Disk reserve and admission"#,
        intro: r#"States per reserve: `Full`, `Partial` (some consumed), `Insufficient` (the target could not be preallocated), `Exhausted` (a lifecycle write needed more than remained)."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                K1,
                r#"S6"#,
                r#"Free space ≥ the reserve targets"#,
                r#"Preallocate `.reserve` in the data root and holder directory, fsync each"#,
                r#"Full"#,
                r#"Recreated next start"#,
                r#"R6-9"#,
                Step
            ),
            row!(
                K1i,
                r#"S6"#,
                r#"Free space < a reserve target"#,
                r#"Preallocate what fits, fsync"#,
                r#"Insufficient; `disk_reserve_insufficient`; startup continues"#,
                r#"Retried by K4"#,
                r#"R7-8"#,
                Step
            ),
            row!(
                K2,
                r#"Full, Partial or Insufficient + a lifecycle write (W, W[x], G for A3/L1/S10, M, Q) gets `ENOSPC`"#,
                r#"The matching reserve holds at least the needed bytes"#,
                r#"Truncate the reserve by the needed amount, fsync → retry the write once"#,
                r#"Partial (or Insufficient); `disk_reserve_used`"#,
                r#"Smaller reserve; K4 refills it"#,
                r#"R6-9"#,
                Step
            ),
            row!(
                K2x,
                r#"any + a lifecycle write gets `ENOSPC`"#,
                r#"The reserve holds less than needed, or the retry failed"#,
                r#"—"#,
                r#"Exhausted. The calling row **fails with no state change**; its earlier durable writes stand per its crash column; `disk_reserve_exhausted`"#,
                r#"—"#,
                r#"R7-8"#,
                Step
            ),
            row!(
                K5,
                r#"Exhausted + any work"#,
                r#"Only work that frees space or writes nothing may run: serving within existing leases; lease lapse (L3); A0's in-memory refusals; deletions of already-claimed or owned units (O6, O11, O12, L5); list fetches (F rows, whose A1 fails at its W until space returns). **Refused at their first W:** builders, A1, R1, R2, T1, T2, L1, O7, O8 and every Q"#,
                r#"—"#,
                r#"Exhausted until K4"#,
                r#"—"#,
                r#"R7-8"#,
                Implicit("every lifecycle write is refused while Exhausted")
            ),
            row!(
                K3,
                r#"A builder requests admission"#,
                r#"Layout + withheld + quarantined + staging bytes + estimate + reserves ≤ `data_root_max_bytes`, and ≤ free space"#,
                r#"—"#,
                r#"Admitted; otherwise paused with `disk_pressure`. **Never** a deletion"#,
                r#"—"#,
                r#"R6-9"#,
                NotModeled("builders")
            ),
            row!(
                K4,
                r#"Partial, Insufficient or Exhausted + space available"#,
                r#"—"#,
                r#"Preallocate up to the target, fsync"#,
                r#"Full, or Partial or Insufficient if still short"#,
                r#"Retried"#,
                r#"R6-9, R7-8"#,
                Step
            ),
        ],
    },
    Machine {
        key: "M10",
        title: r#"Route health (all lifecycle routes)"#,
        intro: r#"States: `Normal`, `AuthHalted(401)`, `AuthHalted(403)`."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                N1,
                r#"Normal + `401 invalid_holder_proof` on any route (F8, R11, T6b). `401 replay` never halts (F8r, R11r, T6r)"#,
                r#"—"#,
                r#"—"#,
                r#"AuthHalted(401): no list, report or status traffic; serving continues until leases lapse; `lifecycle_unauthorized`"#,
                r#"—"#,
                r#"HW §4.5 ("stop, alert"), R7-7"#,
                Step
            ),
            row!(
                N2,
                r#"Normal or AuthHalted(401) + `403 holder_not_authorized` on any route (F8, R11, T6a)"#,
                r#"—"#,
                r#"—"#,
                r#"AuthHalted(403), and C11: nothing servable"#,
                r#"—"#,
                r#"HW §4.5, R7-7"#,
                Step
            ),
            row!(
                N2c,
                r#"N2 enters 403 halt or a handoff reaches its first-byte guard"#,
                r#"Check route health at first byte for report and status; do not rely on enqueue-time health"#,
                r#"W when recording invalidation; close transport gate and cancel unsent bytes without waiting for disk"#,
                r#"Queued attempt is never sent; previously transmitted attempts are kept as uncertain"#,
                r#"No queued bytes survive restart; recover durable possibly-transmitted attempts conservatively"#,
                r#"Astra hard stop"#,
                Focused("403 queue-cut fixture; also explored in general fault profiles")
            ),
            row!(
                N3,
                r#"AuthHalted + any F1, R2, R2h, T2 or T2h trigger"#,
                r#"—"#,
                r#"—"#,
                r#"Refused; unchanged"#,
                r#"—"#,
                r#"R7-7"#,
                Implicit("F1, R2, R2h, T2 and T2h check the route")
            ),
            row!(
                N4,
                r#"AuthHalted + operator resume (a daemon restart after the operator fixes the key, enrollment or authorization; logged as `lifecycle_resume`)"#,
                r#"—"#,
                r#"—"#,
                r#"Process exits; M1 restarts in Normal and F1 fires at S12"#,
                r#"—"#,
                r#"R7-7"#,
                Step
            ),
            row!(
                N5,
                r#"a report or the status `halted` (R9, R17, T6c) + operator `kwiry holder resume-report` (daemon stopped, audited, after the cause is fixed)"#,
                r#"The command holds `holder.lock`"#,
                r#"W (`halted → invalidated`; attempts that may have been transmitted go to `uncertain`)"#,
                r#"Restart; R1 or T1 re-derives under a new key"#,
                r#"Either"#,
                r#"Round-8 #5"#,
                Step
            ),
        ],
    },
    Machine {
        key: "M11",
        title: r#"Root pins (HW §15 H.16)"#,
        intro: r#"Engine room items only. The holder serves a room from artifacts stamped with the item's `root_pin.pin_ref`, built from a root it observes at `root_pin.pin_id`, under a list received in this process. P6–P8 are the authority behaviour the holder relies on."#,
        header: [
            r#"ID"#,
            r#"State + event"#,
            r#"Guard (reads)"#,
            r#"Durable writes"#,
            r#"Next"#,
            r#"Crash after write"#,
            r#"Implements"#,
        ],
        rows: &[
            row!(
                P1,
                r#"A0: an admitted list shows an `active` room with a changed, `null` or history-conflicting `root_pin`"#,
                r#"Compared with the same holding's pin in the list last applied in this process. At process start that baseline is empty, so every non-null pin of the first list is changed and serves only after its A4 (H.24); conflict = the pin-history holds the `pin_id` under another room"#,
                r#"L3 only: refuse the room for retrieval, readiness and end-of-request release at once"#,
                r#"A1. The refusal survives an A1–A3 failure and retries; only a successful A4 broadens serving"#,
                r#"Nothing durable; restart serves nothing until S12"#,
                r#"HW §15 H.16.5 effective point, H.24; AP22, AP25"#,
                Focused("root-pin fixture")
            ),
            row!(
                P2,
                r#"A1 commits a list with pinned room items"#,
                r#"A `pin_id` not yet in the pin-history"#,
                r#"In A1's W with the high-water: add `{pin_id → resource.id}`; never replace or evict a pair. If the pairs would exceed the history ceiling, alert and raise it before persisting (never evict, never renew the lease on failure)"#,
                r#"A2 or A2n"#,
                r#"The high-water and the pairs commit together"#,
                r#"HW §15 H.16.5; AP11–AP11d, AP24, AP27"#,
                Implicit("part of A1's W")
            ),
            row!(
                P3,
                r#"A query, readiness check or end-of-request release for room `r`"#,
                r#"`root_pin` non-null; artifacts stamped with its `pin_ref` by the holder's durable derivation record, never by the in-process baseline (H.24); the root observed at its `pin_id`; no pin-history conflict (AP24 `root_pin_previously_listed`); list received in this process"#,
                r#"—"#,
                r#"Served; otherwise R17 with no results. A `null` pin, a mismatch or a conflict withholds the artifacts and authorizes no deletion. A pin that changed while the holder was down serves no artifact derived under the old `pin_ref` (H10.5 covers only same-ref artifacts)"#,
                r#"—"#,
                r#"HW §15 H.16.5, H.24; AP1, AP6, AP7, AP16, AP18"#,
                Implicit("the serving guard every read evaluates")
            ),
            row!(
                P4,
                r#"Room `active` with a non-null applied pin whose `pin_ref` no artifact carries"#,
                r#"The walk observes the root at `pin_id` (otherwise `root_pin_mismatch`: fail closed, index nothing); no A0 refusal or AP24 conflict; publication re-checks the applied pin"#,
                r#"G (artifacts stamped with `pin_ref`); a storage budget stops new artifact writes and never evicts protected data"#,
                r#"Servable under P3"#,
                r#"An unpublished build is discarded"#,
                r#"HW §15 H.16.5; AP2, AP4, AP5, AP14, AP19"#,
                Focused("root-pin fixture")
            ),
            row!(
                P5,
                r#"Root observed at another `pin_id` (AP3) + the H10.5 stale bound passes"#,
                r#"—"#,
                r#"—"#,
                r#"Until the bound, only previously validated artifacts stamped with the current `pin_ref` may serve (never after an A0 refusal or for AP24); then R17"#,
                r#"—"#,
                r#"HW §15 H.16.5, H10.5; AP3"#,
                Focused("root-pin fixture")
            ),
            row!(
                P6,
                r#"Authority: `attest-root` for room `r` (admin)"#,
                r#"Holding known and `active`; `expect_current` equals the current `pin_ref` or `none`; `pin_id` absent from the id-space history or mapped to `r`"#,
                r#"Journal `root_pin_attested` (fsync) then one transaction: fresh `pin_ref`, history insert, `seq` and `holder_rev` advance; refusals write nothing"#,
                r#"The next list carries the pin; an in-flight cursor gets 410"#,
                r#"Reconcile completes or aborts the appended entry"#,
                r#"HW §15 H.16.1–H.16.4; OA1–OA4, OA8"#,
                Focused("root-pin fixture")
            ),
            row!(
                P7,
                r#"Authority: supported restore replays `root_pin_attested` after the restore point"#,
                r#"In journal order; apply only if the current `pin_ref` equals `prev_pin_ref` and the reuse rule holds"#,
                r#"Pin and history as journaled; a skipped entry is audited; an entry whose holding is gone reserves its `pin_id` as `"*"` in its `provider_scope`, permanently"#,
                r#"The first new-epoch list carries the replayed pins"#,
                r#"—"#,
                r#"HW §15 H.16.3; OA5, OA11"#,
                Focused("root-pin fixture")
            ),
            row!(
                P8,
                r#"Authority: `restore_journal_lost`"#,
                r#"—"#,
                r#"First new-epoch transaction: every `active` engine room's pin becomes `null` (cause `holder-reset`); terminating and retired keep their last pin; history kept"#,
                r#"Each room serves again only after a fresh observation, `attest-root` with `expect_current: none`, and a rebuild under the new `pin_ref`"#,
                r#"—"#,
                r#"HW §15 H.19 item 5; OA7"#,
                Focused("root-pin fixture")
            ),
        ],
    },
];
